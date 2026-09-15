// The qualification a Dislocation candidate goes through: the news pass always,
// and the deep pass — which is all "Qualité A+" means now — when the
// `quality_aplus` option is on.
//
//   liquidity confirmed ─→ Q1 structure + domicile   (pure, no I/O)
//                       ─→ Q2 dilution hard reject   (cache/live filings)
//                       ─→ Q3 LLM news scoring       (flash, ~2-4 s)
//                       ─→ Q4 LLM deep               (pro, ~10-25 s)
//                       ─→ final score ≥ threshold ? alert : logged rejection
//
// With the option off, Q4 does not run and no provider call is ever spent on a
// dossier — `no_dossier_work_without_the_deep_pass` pins that. Q3 runs either way:
// it owns a third of the score.
//
// THE GATES ARE PURE, THE PIPELINE IS NOT. Everything that decides something —
// `structural_gate`, `verdict`, `NewsWatch` — is a pure function over plain data, so
// the interesting behaviour is testable without a market, a database or a model.
// `qualify` is the thin async shell that feeds them.
//
// WHY REJECTIONS ARE TYPED rather than a bool: a candidate that never reached the
// screen is invisible, and the only way to tune the gates afterwards is to know which
// one turned each ticker away. `Rejection::reason` is written to `local_logs` for
// exactly that.

use chrono::{DateTime, Utc};

use crate::dislocation::{live_status, score};
use crate::quality_llm::{Pass, QualityAnswer};
use crate::strategies::issuer_profile::IssuerProfile;
use crate::types::AlertSignal;

// ─── Thresholds ────────────────────────────────────────────────────────────────

/// A+ tightens what gate 1 already allowed, because the bar for "worth an LLM call
/// and a slot on the screen" is higher than the bar for "worth watching".
#[derive(Debug, Clone, Copy)]
pub struct QualityConfig {
    /// Stricter than the engine's 30M: A+ is about tension, and 30M is not tense.
    pub float_max: u64,
    pub price_min: f64,
    pub price_max: f64,
    /// Dollars traded since the session opened. Below this the name cannot absorb a
    /// position, however pretty the chart is.
    pub session_dollar_volume_min: f64,
    /// A reverse split this recent means the float and the price history are both
    /// artefacts, and the pattern is overwhelmingly a failing issuer.
    pub min_days_since_split: i64,
    /// A priced offering filed inside this window is the one hard reject: the issuer
    /// is selling into the move as it happens.
    pub offering_priced_hours: i64,
    /// How long after qualification a candidate keeps being watched for a headline.
    pub news_watch_secs: i64,
    /// Floor between two re-scores of the same symbol.
    pub rescore_min_interval_secs: i64,
    /// Cap on re-scores per symbol per session.
    pub max_rescores: u8,
}

impl QualityConfig {
    pub const DEFAULT: QualityConfig = QualityConfig {
        float_max: 20_000_000,
        price_min: 1.0,
        price_max: 20.0,
        session_dollar_volume_min: 250_000.0,
        min_days_since_split: 5,
        offering_priced_hours: 24,
        news_watch_secs: 30 * 60,
        rescore_min_interval_secs: 60,
        max_rescores: 3,
    };
}

// ─── Q1 / Q2: deterministic gates ──────────────────────────────────────────────

/// Why a candidate was turned away before (or after) the model saw it.
#[derive(Debug, Clone, PartialEq)]
pub enum Rejection {
    Float {
        shares: u64,
    },
    Price {
        price: f64,
    },
    DollarVolume {
        dollars: f64,
    },
    BelowVwap {
        price: f64,
        vwap: f64,
    },
    RecentSplit {
        days: i64,
    },
    OfferingPriced,
    /// The model answered, and the final score fell short.
    BelowThreshold {
        score: f64,
        threshold: f64,
    },
    /// The model could not be reached. Distinct from a low score on purpose: this is
    /// our failure, not the ticker's.
    ScoringFailed {
        error: String,
    },
}

impl Rejection {
    /// One line for `local_logs`, so a candidate that never reached the screen can
    /// still be accounted for.
    pub fn reason(&self) -> String {
        match self {
            Rejection::Float { shares } => format!("float {shares} > max"),
            Rejection::Price { price } => format!("prix ${price:.2} hors bande"),
            Rejection::DollarVolume { dollars } => {
                format!("volume de session {:.0} k$ insuffisant", dollars / 1_000.0)
            }
            Rejection::BelowVwap { price, vwap } => {
                format!("prix ${price:.2} sous le VWAP ${vwap:.2}")
            }
            Rejection::RecentSplit { days } => format!("split il y a {days} j"),
            Rejection::OfferingPriced => "offering tarifé dans les dernières 24 h".into(),
            Rejection::BelowThreshold { score, threshold } => {
                format!("score {score:.0} < seuil {threshold:.0}")
            }
            Rejection::ScoringFailed { error } => format!("scoring indisponible: {error}"),
        }
    }

    /// Whether the candidate could still qualify later in the session. A structural
    /// fact (float, a split) will not change; a score can, once news lands.
    pub fn is_final(&self) -> bool {
        matches!(
            self,
            Rejection::Float { .. } | Rejection::RecentSplit { .. } | Rejection::Price { .. }
        )
    }
}

/// What Q1 reads. Assembled from the live snapshot by the caller.
#[derive(Debug, Clone, Copy)]
pub struct Structure {
    pub price: f64,
    pub float_shares: Option<u64>,
    pub session_dollar_volume: Option<f64>,
    pub session_vwap: Option<f64>,
    pub days_since_split: Option<i64>,
}

/// Q1 — structural fitness. Unknown values do NOT reject: the metadata pipeline fills
/// asynchronously, and refusing a ticker because our own cache is cold would silently
/// bias the whole strategy toward well-covered names, which are exactly the ones that
/// don't run. Only a KNOWN disqualifying value rejects.
pub fn structural_gate(s: &Structure, cfg: &QualityConfig) -> Result<(), Rejection> {
    if !(cfg.price_min..=cfg.price_max).contains(&s.price) {
        return Err(Rejection::Price { price: s.price });
    }
    if let Some(f) = s.float_shares {
        if f > cfg.float_max {
            return Err(Rejection::Float { shares: f });
        }
    }
    if let Some(d) = s.session_dollar_volume {
        if d < cfg.session_dollar_volume_min {
            return Err(Rejection::DollarVolume { dollars: d });
        }
    }
    if let Some(v) = s.session_vwap {
        if s.price < v {
            return Err(Rejection::BelowVwap {
                price: s.price,
                vwap: v,
            });
        }
    }
    if let Some(d) = s.days_since_split {
        if d < cfg.min_days_since_split {
            return Err(Rejection::RecentSplit { days: d });
        }
    }
    Ok(())
}

/// Q2 — the dilution hard reject. Everything else about dilution (a shelf, an ATM on
/// a funded company, a history of raising) is a SIGNAL that goes into the payload and
/// the score, not a veto: only "the issuer priced an offering into this move" is
/// mechanical enough to decide on its own.
pub fn dilution_gate(
    offering_priced_recently: bool,
    _cfg: &QualityConfig,
) -> Result<(), Rejection> {
    if offering_priced_recently {
        return Err(Rejection::OfferingPriced);
    }
    Ok(())
}

// ─── The verdict ───────────────────────────────────────────────────────────────

/// A scored candidate.
#[derive(Debug, Clone)]
pub struct Verdict {
    pub pass: Pass,
    pub answer: QualityAnswer,
    /// The three thirds. `news` is filled in from this pass's answer, decayed by the
    /// headline's age; the two tape thirds come from the tick that nominated the
    /// candidate and are not recomputed here.
    pub scores: score::Scores,
    /// `scores.total()`, kept as a field so the persisted row and the log line read
    /// the same number.
    pub final_score: f64,
    pub threshold: f64,
    /// None when the candidate cleared the threshold.
    pub rejection: Option<Rejection>,
}

impl Verdict {
    pub fn passed(&self) -> bool {
        self.rejection.is_none()
    }

    /// The audit line: the model's note on the news, the three thirds, the result
    /// and the bar it was measured against. Written to `local_logs` for pass and
    /// fail alike.
    pub fn trace(&self, symbol: &str) -> String {
        format!(
            "dislocation {symbol} [{}] news modèle {:.0} · {} / seuil {:.0} · {}",
            self.pass.as_str(),
            self.answer.quality_score,
            self.scores.describe(),
            self.threshold,
            match &self.rejection {
                Some(r) => r.reason(),
                None => "retenu".to_string(),
            },
        )
    }
}

/// Put the model's note on the news into the third that was waiting for it, then
/// measure the total against the user's threshold. Pure — the whole scoring
/// decision is inspectable without a model.
///
/// There is no adjustment step any more. A hot industry, a risk domicile, a recent
/// reverse split and an armed ATM used to move this number by fixed points; they
/// are now either universe filters the user switches on, or context the trader
/// reads. Nothing but the three thirds decides.
///
/// A `base.news` that is ALREADY decided is kept as it is: that is how the deep
/// pass runs without re-grading the headline on a rubric that grades something
/// else. See `score_and_publish`.
pub fn verdict(
    pass: Pass,
    answer: QualityAnswer,
    base: score::Scores,
    news_age_minutes: Option<i64>,
    threshold: f64,
) -> Verdict {
    let scores = score::Scores {
        news: Some(base.news.unwrap_or_else(|| {
            score::news_score(
                answer.quality_score,
                news_age_minutes.unwrap_or(score::NEWS_MAX_MINUTES),
            )
        })),
        ..base
    };
    let final_score = scores.total();
    let rejection = (final_score < threshold).then(|| Rejection::BelowThreshold {
        score: final_score,
        threshold,
    });
    Verdict {
        pass,
        answer,
        scores,
        final_score,
        threshold,
        rejection,
    }
}

// ─── The news watch ────────────────────────────────────────────────────────────

/// Per-symbol state for the continuous news watch.
///
/// A catalyst does not always arrive before the move — a halt resolution, a delayed
/// wire, a headline that lands three minutes into the run. The engine therefore keeps
/// watching after qualification, and re-scores the moment a new headline appears.
///
/// The case that matters most is the REJECTED candidate: a ticker turned away for
/// having no news stays under watch, and if the missing headline lands four minutes
/// later, the re-score can put it on screen. Dropping rejected candidates would make
/// the whole watch pointless for exactly the scenario it exists for.
#[derive(Debug, Clone)]
pub struct NewsWatch {
    /// Watching stops here (qualification + `news_watch_secs`, capped by the session).
    pub until: DateTime<Utc>,
    /// Headline count last time we looked. A rise means something landed.
    pub seen_news: usize,
    pub last_score_at: DateTime<Utc>,
    pub rescores: u8,
}

impl NewsWatch {
    pub fn new(now: DateTime<Utc>, seen_news: usize, cfg: &QualityConfig) -> Self {
        Self {
            until: now + chrono::Duration::seconds(cfg.news_watch_secs),
            seen_news,
            last_score_at: now,
            rescores: 0,
        }
    }

    /// Whether a fresh headline warrants a re-score right now. Bounded three ways —
    /// the watch window, a minimum interval, and a per-session cap — because a wire
    /// that publishes a story and two corrections would otherwise cost three model
    /// calls in ten seconds.
    pub fn should_rescore(
        &self,
        now: DateTime<Utc>,
        news_count: usize,
        cfg: &QualityConfig,
    ) -> bool {
        news_count > self.seen_news
            && now <= self.until
            && self.rescores < cfg.max_rescores
            && (now - self.last_score_at).num_seconds() >= cfg.rescore_min_interval_secs
    }

    /// Record that a re-score just ran.
    pub fn mark_rescored(&mut self, now: DateTime<Utc>, news_count: usize) {
        self.seen_news = news_count;
        self.last_score_at = now;
        self.rescores += 1;
    }

    pub fn expired(&self, now: DateTime<Utc>) -> bool {
        now > self.until
    }
}

/// Which passes to run. The news pass always runs — it owns a third of the score.
/// "Qualité A+" is now exactly one thing: the optional second opinion on top.
pub fn passes_for(deep_enabled: bool) -> &'static [Pass] {
    if deep_enabled {
        &[Pass::News, Pass::Deep]
    } else {
        &[Pass::News]
    }
}

fn may_continue_after(pass: Pass, _passed: bool) -> bool {
    // When deep analysis is selected it is a genuine second opinion and the final
    // decision. A news-pass rejection must not silently disable the checked option.
    pass == Pass::News
}

// ─── Alert decoration ──────────────────────────────────────────────────────────

/// Prefix an alert with its A+ verdict, keeping the engine's own rendering intact.
///
/// Additive on purpose: the qualification layer never rebuilds the alert text, it
/// only prepends what it learned. That keeps `make_alert` the single author of the
/// detection half, and means a change to either side can't silently drop the other.
///
/// A candidate that scored below the threshold AFTER already being shown (the deep
/// pass disagreeing with the news pass, or a re-score on stale news) is DEMOTED to
/// priority 4 rather than pulled: a row vanishing from under the trader's cursor is
/// worse than a row that says it got worse.
pub fn decorate(alert: &mut AlertSignal, v: &Verdict) {
    let mut bits = vec![format!(
        "{:.0} · RVOL {:.0} · attention {:.0} · news {:.0}",
        v.final_score,
        v.scores.rvol,
        v.scores.attention,
        v.scores.news.unwrap_or(0.0)
    )];
    if v.pass == Pass::Deep {
        bits.push("A+ double passe".to_string());
    }
    if let Some(f) = &v.answer.news_freshness {
        bits.push(f.clone());
    }
    if let Some(n) = &v.answer.news_verdict {
        if v.answer.news_freshness.as_deref() != Some(n.as_str()) {
            bits.push(n.clone());
        }
    }
    if let Some(d) = &v.answer.dilution_risk {
        bits.push(format!("dilution {d}"));
    }
    if !v.passed() {
        bits.push("sous seuil".to_string());
        alert.priority = 4;
    }
    alert.reason = format!("[{}] {}", bits.join(" · "), alert.reason);
}

/// Mark an alert as awaiting its score, for the `alert_before_score` mode where the
/// row is published the moment liquidity confirms.
pub fn decorate_pending(alert: &mut AlertSignal) {
    alert.reason = format!("[Score …] {}", alert.reason);
}

/// Prefix an alert that was never sent to a model — no headline inside the freshness
/// window, so the news third is a measured zero and the two tape thirds decided on
/// their own. Says so explicitly: "no news" and "not analysed yet" must never look
/// the same on the row.
pub fn decorate_scores(alert: &mut AlertSignal, s: &score::Scores, news_age_minutes: Option<i64>) {
    let news = match news_age_minutes {
        None => "aucune news".to_string(),
        Some(m) if m < 0 => "news horodatée dans le futur".to_string(),
        Some(m) => format!("news de {m} min, hors fenêtre"),
    };
    alert.reason = format!(
        "[{:.0} · RVOL {:.0} · attention {:.0} · {news}] {}",
        s.total(),
        s.rvol,
        s.attention,
        alert.reason
    );
}

#[cfg(test)]
mod decorate_tests {
    use super::*;
    use crate::types::{Session, Side};

    fn alert() -> AlertSignal {
        AlertSignal {
            alert_id: "dl-1-TEST".into(),
            timestamp: Utc::now(),
            symbol: "TEST".into(),
            strategy_id: "dislocation".into(),
            strategy_name: "Dislocation".into(),
            priority: 5,
            session: Session::Premarket,
            price: Some(5.30),
            bid: None,
            ask: None,
            spread: None,
            volume: Some(500_000),
            rvol: Some(12.0),
            change_day_pct: Some(10.0),
            float_shares: Some(4_000_000),
            news_today: true,
            halted: Some(false),
            latency_ui_ms: None,
            reason: "Dislocation — départ 10s : +1.5%".into(),
            display_timeframe: Some("10s".into()),
            side: Some(Side::Long),
            tempo_depth: Some(vec![10, 20]),
        }
    }

    fn scored(score: f64, threshold: f64) -> Verdict {
        verdict(
            Pass::News,
            QualityAnswer {
                quality_score: score,
                news_verdict: Some("solide".into()),
                news_freshness: Some("breaking".into()),
                ..Default::default()
            },
            score::Scores {
                rvol: score,
                attention: score,
                news: None,
            },
            Some(2),
            threshold,
        )
    }

    #[test]
    fn a_passing_verdict_prefixes_without_touching_the_detection_text() {
        let mut a = alert();
        let original = a.reason.clone();
        decorate(&mut a, &scored(82.0, 70.0));
        assert!(
            a.reason
                .starts_with("[82 · RVOL 82 · attention 82 · news 82 · breaking · solide]"),
            "{}",
            a.reason
        );
        assert!(
            a.reason.ends_with(&original),
            "the engine's own text is preserved"
        );
        assert_eq!(a.priority, 5);
    }

    /// Pulling a row out from under the trader is worse than showing it downgraded.
    #[test]
    fn a_failing_verdict_demotes_rather_than_erases() {
        let mut a = alert();
        decorate(&mut a, &scored(52.0, 70.0));
        assert_eq!(a.priority, 4);
        assert!(a.reason.contains("sous seuil"), "{}", a.reason);
        assert!(
            a.reason.contains("départ 10s"),
            "the alert is still readable"
        );
    }

    #[test]
    fn a_deep_verdict_makes_the_second_call_visible() {
        let mut deep = scored(82.0, 70.0);
        deep.pass = Pass::Deep;
        let mut a = alert();
        decorate(&mut a, &deep);
        assert!(a.reason.contains("A+ double passe"), "{}", a.reason);
    }

    #[test]
    fn the_pending_badge_marks_a_row_whose_score_has_not_landed() {
        let mut a = alert();
        decorate_pending(&mut a);
        assert!(a.reason.starts_with("[Score …]"), "{}", a.reason);
        assert_eq!(a.priority, 5, "pending is not a downgrade");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    const CFG: QualityConfig = QualityConfig::DEFAULT;

    fn t(secs: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 20, 8, 30, 0).unwrap() + chrono::Duration::seconds(secs)
    }

    fn good() -> Structure {
        Structure {
            price: 5.30,
            float_shares: Some(4_000_000),
            session_dollar_volume: Some(2_000_000.0),
            session_vwap: Some(5.05),
            days_since_split: Some(400),
        }
    }

    /// The two tape thirds a tick would have handed over.
    fn tape(rvol: f64, attention: f64) -> score::Scores {
        score::Scores {
            rvol,
            attention,
            news: None,
        }
    }

    fn answer(score: f64) -> QualityAnswer {
        QualityAnswer {
            quality_score: score,
            ..Default::default()
        }
    }

    #[test]
    fn a_clean_candidate_clears_the_structural_gate() {
        assert_eq!(structural_gate(&good(), &CFG), Ok(()));
    }

    #[test]
    fn each_structural_rule_rejects_for_its_own_stated_reason() {
        let cases: [(Structure, Rejection); 5] = [
            (
                Structure {
                    float_shares: Some(25_000_000),
                    ..good()
                },
                Rejection::Float { shares: 25_000_000 },
            ),
            (
                Structure {
                    price: 24.0,
                    ..good()
                },
                Rejection::Price { price: 24.0 },
            ),
            (
                Structure {
                    session_dollar_volume: Some(80_000.0),
                    ..good()
                },
                Rejection::DollarVolume { dollars: 80_000.0 },
            ),
            (
                Structure {
                    price: 4.90,
                    session_vwap: Some(5.05),
                    ..good()
                },
                Rejection::BelowVwap {
                    price: 4.90,
                    vwap: 5.05,
                },
            ),
            (
                Structure {
                    days_since_split: Some(2),
                    ..good()
                },
                Rejection::RecentSplit { days: 2 },
            ),
        ];
        for (s, expected) in cases {
            assert_eq!(structural_gate(&s, &CFG), Err(expected.clone()));
            assert!(!expected.reason().is_empty());
        }
    }

    /// The metadata pipeline fills asynchronously. Rejecting on a cold cache would
    /// quietly bias the strategy toward well-covered names — which are precisely the
    /// ones that don't run.
    #[test]
    fn unknown_values_never_reject() {
        let unknown = Structure {
            price: 5.30,
            float_shares: None,
            session_dollar_volume: None,
            session_vwap: None,
            days_since_split: None,
        };
        assert_eq!(structural_gate(&unknown, &CFG), Ok(()));
    }

    /// The A+ structural gate is stricter than the engine's own universe rule, on
    /// purpose: 30M shares is not a tense float, whatever else is true. Pinned
    /// against the ENGINE'S OPTIONS rather than another strategy's compiled config —
    /// this layer belongs to Dislocation and must not drift with Micro Pullback.
    #[test]
    fn a_plus_is_stricter_than_the_engines_universe() {
        let universe = crate::dislocation::DislocationOptions::default();
        assert!(CFG.float_max < universe.float_max);
        assert!(CFG.price_max < universe.price_max);
        // A ticker the universe admits can still be turned away here.
        let s = Structure {
            float_shares: Some(28_000_000),
            ..good()
        };
        assert!(s.float_shares.is_some_and(|f| f <= universe.float_max));
        assert!(structural_gate(&s, &CFG).is_err());
    }

    #[test]
    fn only_a_priced_offering_vetoes_on_dilution() {
        assert_eq!(dilution_gate(false, &CFG), Ok(()));
        assert_eq!(dilution_gate(true, &CFG), Err(Rejection::OfferingPriced));
    }

    /// A structural fact won't change today; a score can, once news lands. The news
    /// watch uses this to decide who is worth keeping an eye on.
    #[test]
    fn only_structural_rejections_are_final() {
        assert!(Rejection::Float { shares: 99 }.is_final());
        assert!(Rejection::RecentSplit { days: 1 }.is_final());
        assert!(!Rejection::BelowThreshold {
            score: 40.0,
            threshold: 70.0
        }
        .is_final());
        assert!(!Rejection::OfferingPriced.is_final());
        assert!(!Rejection::ScoringFailed {
            error: "timeout".into()
        }
        .is_final());
    }

    /// The model owns exactly one third, and the two tape thirds are not re-derived
    /// here — they are the tick's, carried on the candidate.
    #[test]
    fn the_verdict_is_the_mean_of_three_thirds() {
        let v = verdict(Pass::News, answer(90.0), tape(90.0, 90.0), Some(0), 70.0);
        assert_eq!(
            v.answer.quality_score, 90.0,
            "the model's own note is preserved"
        );
        assert_eq!(v.scores.news, Some(90.0));
        assert_eq!(v.final_score, 90.0);
        assert!(v.passed());

        // A perfect catalyst cannot rescue a tape nobody is watching.
        let quiet = verdict(Pass::News, answer(100.0), tape(0.0, 0.0), Some(0), 70.0);
        assert_eq!(quiet.final_score.round(), 33.0);
        assert_eq!(
            quiet.rejection,
            Some(Rejection::BelowThreshold {
                score: quiet.final_score,
                threshold: 70.0
            })
        );
    }

    /// The age decay is the engine's, not the model's — the rubric is explicitly
    /// told not to apply it, so it has to be applied here.
    #[test]
    fn the_news_third_is_decayed_by_the_headlines_age() {
        let fresh = verdict(Pass::News, answer(90.0), tape(60.0, 60.0), Some(2), 0.0);
        let tepid = verdict(Pass::News, answer(90.0), tape(60.0, 60.0), Some(15), 0.0);
        assert_eq!(fresh.scores.news, Some(90.0));
        assert!(tepid.scores.news.unwrap() < 50.0);
        assert!(tepid.final_score < fresh.final_score);
        // A missing age is treated as the ceiling: no headline, no third.
        let unknown = verdict(Pass::News, answer(90.0), tape(60.0, 60.0), None, 0.0);
        assert_eq!(unknown.scores.news, Some(0.0));
    }

    /// With "Qualité A+" off, no candidate must cost a filings/financials call:
    /// the news rubric grades the catalyst and never reads the dossier. The
    /// invariant is structural — `passes_for` is the only thing that can ask for
    /// `GatherPlan::DEEP` — so it is pinned here rather than left to inspection.
    #[test]
    fn no_dossier_work_without_the_deep_pass() {
        assert_eq!(passes_for(false), &[Pass::News]);
        assert!(
            !passes_for(false).contains(&Pass::Deep),
            "only the deep pass requests GatherPlan::DEEP, the one plan that hits \
             SEC and Massive for filings and statements"
        );
        assert_eq!(passes_for(true), &[Pass::News, Pass::Deep]);
        assert!(!intel_live::GatherPlan::NEWS.refresh_company_intel);
        assert!(!intel_live::GatherPlan::NEWS_ONLY.refresh_company_intel);
        assert!(!intel_live::GatherPlan::CACHE_ONLY.refresh_company_intel);
        assert!(intel_live::GatherPlan::DEEP.refresh_company_intel);
    }

    #[test]
    fn the_threshold_is_the_users_not_a_constant() {
        let a = answer(64.0);
        let t = tape(64.0, 64.0);
        assert!(verdict(Pass::News, a.clone(), t, Some(1), 60.0).passed());
        assert!(!verdict(Pass::News, a, t, Some(1), 70.0).passed());
    }

    #[test]
    fn the_trace_names_all_three_thirds_and_the_bar() {
        let v = verdict(Pass::Deep, answer(66.0), tape(40.0, 80.0), Some(0), 70.0);
        let t = v.trace("TEST");
        assert!(t.contains("TEST"), "{t}");
        assert!(t.contains("deep"), "{t}");
        assert!(t.contains("news modèle 66"), "{t}");
        assert!(t.contains("RVOL 40"), "{t}");
        assert!(t.contains("attention 80"), "{t}");
        assert!(t.contains("news 66"), "{t}");
        assert!(t.contains("seuil 70"), "{t}");
    }

    // ── News watch ──

    #[test]
    fn a_new_headline_triggers_a_rescore() {
        let w = NewsWatch::new(t(0), 1, &CFG);
        assert!(
            !w.should_rescore(t(90), 1, &CFG),
            "no new headline, nothing to do"
        );
        assert!(w.should_rescore(t(90), 2, &CFG), "a headline landed");
    }

    /// A wire that publishes a story and two corrections must not cost three model
    /// calls in ten seconds.
    #[test]
    fn rescores_are_rate_limited_and_capped() {
        let mut w = NewsWatch::new(t(0), 1, &CFG);
        assert!(
            !w.should_rescore(t(10), 2, &CFG),
            "too soon after the last score"
        );
        assert!(w.should_rescore(t(60), 2, &CFG));

        for i in 0..CFG.max_rescores {
            let at = t(60 * (i as i64 + 1));
            assert!(w.should_rescore(at, 2 + i as usize, &CFG), "rescore {i}");
            w.mark_rescored(at, 2 + i as usize);
        }
        assert!(
            !w.should_rescore(t(600), 99, &CFG),
            "the per-session cap holds even for a genuinely new headline"
        );
    }

    #[test]
    fn the_watch_expires_with_its_window() {
        let w = NewsWatch::new(t(0), 0, &CFG);
        assert!(!w.expired(t(CFG.news_watch_secs - 1)));
        assert!(w.expired(t(CFG.news_watch_secs + 1)));
        assert!(
            !w.should_rescore(t(CFG.news_watch_secs + 1), 5, &CFG),
            "a headline after the window closes is not this alert's business"
        );
    }

    #[test]
    fn deep_analysis_is_the_second_pass_and_can_be_switched_off() {
        assert_eq!(passes_for(true), &[Pass::News, Pass::Deep]);
        assert_eq!(passes_for(false), &[Pass::News]);
        assert!(may_continue_after(Pass::News, true));
        assert!(
            may_continue_after(Pass::News, false),
            "the selected deep pass is the final decision, even after the news pass rejects"
        );
        assert!(!may_continue_after(Pass::Deep, true));
    }

    #[test]
    fn deep_timeout_covers_the_measured_healthy_latency() {
        assert!(DEEP_SCORE_TIMEOUT >= std::time::Duration::from_secs(90));
    }

    #[test]
    fn unchanged_news_version_is_claimed_only_once() {
        let now = t(42);
        assert!(claim_score("CACHE-ONLY-TEST", now, 0));
        assert!(!claim_score("CACHE-ONLY-TEST", now, 0));
        assert!(claim_score("CACHE-ONLY-TEST", now, 1));
    }
}

// ─── The async pipeline ────────────────────────────────────────────────────────

use std::collections::HashMap;
use std::sync::OnceLock;
use std::sync::{Arc, Mutex, RwLock};

use crate::config::secrets::Secrets;
use crate::config::AppConfig;
use crate::market_state::MarketState;
use crate::dislocation::intel_live::{self, LiveIntel};
use crate::dislocation::DislocationOptions;
use crate::quality_llm::payload as pl;
use crate::types::TapeKind;

/// A burst of departures at 04:05 must not fan out into fifty concurrent SEC and
/// Deepseek calls. Four at a time keeps us inside every provider's fair-use policy
/// while still qualifying a realistic morning's candidates promptly.
static QUALIFY_SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);
static SCORE_CLAIMS: OnceLock<Mutex<HashMap<(String, String, u64), usize>>> = OnceLock::new();
/// A+ is an alert-time filter, not a batch report. The shared Deepseek client allows
/// five minutes for heavier jobs. The deep model has measured healthy latencies up
/// to ~82 seconds, so 45 seconds was not a failure budget: it was cutting valid
/// answers off mid-analysis, especially visibly during accelerated replay.
const NEWS_SCORE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
const DEEP_SCORE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);
const MASSIVE_NEWS_POLL_SECS: i64 = 5 * 60;

/// Claim the newest visible catalyst version for one ticker/day. Both the event
/// engine and the 30-minute news watcher pass through here, preventing a second
/// event and a watcher wake-up from paying for the same LLM reads concurrently.
pub fn claim_score(symbol: &str, now: DateTime<Utc>, visible_news: usize) -> bool {
    // Replay generation is part of the identity: replaying the same ticker/day a
    // second time is a new training run, not a duplicate live event.
    let key = (
        symbol.to_ascii_uppercase(),
        crate::time::et_date(now),
        crate::replay::clock::generation(),
    );
    let claims = SCORE_CLAIMS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut claims = claims.lock().unwrap();
    claims.retain(|(_, day, generation), _| day == &key.1 && *generation == key.2);
    match claims.get(&key) {
        Some(seen) if *seen >= visible_news => false,
        _ => {
            claims.insert(key, visible_news);
            true
        }
    }
}

/// Everything the qualification needs from the detection side. Passed explicitly
/// rather than re-derived, so the payload describes the departure that actually
/// fired rather than whatever the market looks like a few seconds later.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub alert: AlertSignal,
    pub tempo_secs: i64,
    pub tempo_depth: Vec<i64>,
    pub price_return_pct: f64,
    pub volume_ratio: f64,
    pub trade_rate_ratio: Option<f64>,
    pub range_ratio: f64,
    pub tape_source: TapeKind,
    pub tape_rate_per_sec: Option<f64>,
    pub tape_dollar_per_sec: f64,
    pub tape_active_seconds: Option<u32>,
    pub tape_window_secs: i64,
    pub detected_at: DateTime<Utc>,
    /// The two tape thirds, as measured by the tick that nominated this candidate.
    /// Carried rather than recomputed so the number in the log, the number on the
    /// row and the number in the database are the same object.
    pub scores: score::Scores,
    /// Age of the headline about to be graded, in minutes at detection time.
    pub news_age_minutes: Option<i64>,
}

/// What the A+ overlay reads: every judgement of the current day/mode, and whatever
/// the pipeline has in flight. One shape, one poll, one consistent instant.
#[derive(Debug, Clone, serde::Serialize)]
pub struct QualityBoard {
    pub rows: Vec<crate::local_db::quality_repository::QualityRow>,
    pub live: Vec<live_status::LiveStatus>,
}

/// Shared handles the pipeline needs. Bundled because passing nine `Arc`s through
/// three call layers is how a signature becomes unreadable.
#[derive(Clone)]
pub struct Deps {
    pub db: Arc<Mutex<rusqlite::Connection>>,
    pub config: Arc<RwLock<AppConfig>>,
    pub secrets: Arc<RwLock<Secrets>>,
    pub market: Arc<RwLock<MarketState>>,
    pub active_alerts: Arc<RwLock<Vec<AlertSignal>>>,
    pub alert_history: Arc<RwLock<Vec<AlertSignal>>>,
}

/// Qualify one candidate, publish or refuse it, then keep watching for news.
///
/// Spawned as its own task: the engine's loop runs every second and must never wait
/// on an API call. The task owns the whole lifecycle of one candidate — score,
/// publish, watch, re-score — so there is no shared mutable state between it and the
/// loop beyond the alert list itself.
pub async fn run_qualification(deps: Deps, cand: Candidate, opts: DislocationOptions) {
    let cfg = QualityConfig::DEFAULT;
    let symbol = cand.alert.symbol.clone();

    // In `alert_before_score` mode the row goes up immediately and the scores update
    // it in place; in the default blocking mode nothing is shown until a score lands.
    if opts.alert_before_score {
        let mut pending = cand.alert.clone();
        decorate_pending(&mut pending);
        // The row appears immediately, but the phone contract remains stricter:
        // under A+ it rings only after a score actually clears the threshold.
        crate::scanner::push_alert(&deps.active_alerts, &deps.alert_history, pending);
    }

    // The pane must say "being analysed", not stay empty, from the instant the
    // candidate exists — the wait for a slot plus the two passes is easily half a
    // minute, and an empty pane reads as "never a candidate".
    live_status::set_phase(&symbol, live_status::Phase::Queued);

    // Only live-provider + LLM work consumes a slot. The 30-minute news watcher
    // below is idle almost all of its life and must not monopolise one of the four
    // qualification slots (four early candidates used to block the fifth for the
    // whole watch window).
    let slot = match QUALIFY_SLOTS.acquire().await {
        Ok(s) => s,
        Err(_) => {
            live_status::clear_phase(&symbol);
            return;
        }
    };

    // ── Q2 + payload: one live gather, reused by every later re-score ──
    //
    // The float is fetched only when the deep pass will actually read it. The news
    // payload does not carry a float — it carries headlines — so with "Qualité A+"
    // off that call would buy nothing.
    let mut intel = intel_live::gather(
        &deps.db,
        &deps.config,
        &deps.secrets,
        &symbol,
        intel_live::NEWS_BUDGET,
        if opts.quality_aplus {
            intel_live::GatherPlan::NEWS
        } else {
            intel_live::GatherPlan::NEWS_ONLY
        },
    )
    .await;

    // Price, VWAP, session volume, recent splits and the old 20M A+ float ceiling
    // are intentionally NOT re-gated here. V2 owns one explicit universe rule
    // (current Settings price band + <=100M known float); A+ starts at catalyst
    // quality. The fields remain in the payload as context, never hidden vetoes.
    if let Err(r) = dilution_gate(
        intel.offering_priced_within_hours(cfg.offering_priced_hours, market_now(&deps)),
        &cfg,
    ) {
        refuse(&deps, &opts, &cand, &symbol, &r, opts.alert_before_score);
        live_status::clear_phase(&symbol);
        return;
    }

    let profile = issuer_profile(&intel);
    let massive_only_news = {
        let s = deps.secrets.read().unwrap();
        let alpaca_ready = s
            .alpaca_key
            .as_deref()
            .is_some_and(|v| !v.trim().is_empty())
            && s.alpaca_secret
                .as_deref()
                .is_some_and(|v| !v.trim().is_empty());
        !alpaca_ready
            && s.massive_api_key
                .as_deref()
                .is_some_and(|v| !v.trim().is_empty())
    };
    let initial_market_now = market_now(&deps);
    let mut newest_rest_news = intel
        .news
        .iter()
        .filter(|n| n.created_at <= initial_market_now)
        .map(|n| n.created_at)
        .max();
    let mut last_rest_poll_at = cand.detected_at;
    let api_key = deps.secrets.read().unwrap().deepseek_api_key.clone();
    let Some(api_key) = api_key.filter(|k| !k.trim().is_empty()) else {
        // In blocking A+ mode an unscored ticker is not an A+ alert. Publishing it
        // here used to make the checkbox appear disabled and let noise through.
        log(
            &deps,
            "warn",
            &format!("dislocation {symbol}: pas de clé Deepseek, candidat non publié"),
        );
        update_event_outcome(&deps, &cand.alert.alert_id, "a_plus_no_key_closed");
        live_status::clear_phase(&symbol);
        if opts.alert_before_score {
            let mut unavailable = cand.alert.clone();
            unavailable.priority = 4;
            unavailable.reason = format!(
                "[A+ score indisponible · clé absente] {}",
                unavailable.reason
            );
            crate::scanner::push_alert(&deps.active_alerts, &deps.alert_history, unavailable);
        }
        return;
    };

    // ── Q3/Q4: score, publish, then watch for news ──
    let mut watch = NewsWatch::new(cand.detected_at, news_count(&deps, &symbol), &cfg);
    let mut published = score_and_publish(
        &deps,
        &cand,
        &intel,
        &profile,
        &opts,
        &api_key,
        true,
        opts.alert_before_score,
    )
    .await;
    drop(slot);
    // Between passes nothing is in flight: the persisted verdict on screen is
    // current, and a spinner left up here would claim otherwise for 30 minutes.
    live_status::clear_phase(&symbol);

    if !opts.news_watch {
        return;
    }

    // A catalyst does not always arrive before the move. Keep watching — including
    // for a candidate that was just turned away for having no news, which is the
    // whole point: the headline that was missing can still land.
    let watch_wall_started = std::time::Instant::now();
    while !watch.expired(market_now(&deps))
        && watch_wall_started.elapsed() < std::time::Duration::from_secs(45 * 60)
    {
        crate::replay::clock::scaled_sleep(1_000).await;
        let now = market_now(&deps);
        let count = news_count(&deps, &symbol);
        let websocket_due = watch.should_rescore(now, count, &cfg);
        let rest_due = massive_only_news
            && watch.rescores < cfg.max_rescores
            && (now - last_rest_poll_at).num_seconds() >= MASSIVE_NEWS_POLL_SECS;
        if !websocket_due && !rest_due {
            continue;
        }
        if rest_due {
            last_rest_poll_at = now;
        }
        let websocket_claimed = if websocket_due {
            watch.mark_rescored(now, count);
            claim_score(&symbol, now, count)
        } else {
            false
        };
        if !websocket_claimed && !rest_due {
            continue;
        }
        let _rescore_slot = match QUALIFY_SLOTS.acquire().await {
            Ok(s) => s,
            Err(_) => {
                live_status::clear_phase(&symbol);
                return;
            }
        };
        // Re-fetch the news half only; filings and financials don't change in the
        // minutes after a headline, and re-hitting SEC per headline would be abusive.
        let fresh = intel_live::gather(
            &deps.db,
            &deps.config,
            &deps.secrets,
            &symbol,
            intel_live::NEWS_BUDGET,
            intel_live::GatherPlan::NEWS_ONLY,
        )
        .await;
        let fresh_rest_news = fresh
            .news
            .iter()
            .filter(|n| n.created_at <= now)
            .map(|n| n.created_at)
            .max();
        let rest_changed = rest_due
            && fresh_rest_news
                .is_some_and(|latest| newest_rest_news.map_or(true, |previous| latest > previous));
        if rest_changed {
            newest_rest_news = fresh_rest_news;
            if !websocket_due {
                let synthetic_version = watch.seen_news.saturating_add(1);
                watch.mark_rescored(now, synthetic_version);
                let _ = claim_score(&symbol, now, synthetic_version);
            }
        }
        if !websocket_claimed && !rest_changed {
            continue;
        }
        intel.news = fresh.news;
        intel.news_source = fresh.news_source;
        // The headline the trader can already read beats the summary of a score
        // computed before it existed. Publish it raw, with the spinner, and let the
        // re-score replace it when it lands.
        if let Some(latest) = intel
            .news
            .iter()
            .filter(|n| n.created_at <= now)
            .max_by_key(|n| n.created_at)
        {
            live_status::set_pending_headline(&symbol, &latest.headline, latest.created_at);
        }
        live_status::set_phase(&symbol, live_status::Phase::Rescoring);
        log(
            &deps,
            "info",
            &format!(
                "dislocation {symbol}: news détectée, re-scoring ({}/{})",
                watch.rescores, cfg.max_rescores
            ),
        );
        published = score_and_publish(
            &deps, &cand, &intel, &profile, &opts, &api_key, false, published,
        )
        .await;
        live_status::clear_phase(&symbol);
    }
    live_status::clear_phase(&symbol);
}

/// Run the configured passes and publish/refuse accordingly. Returns whether the
/// candidate ended up on screen.
///
/// With deep analysis selected, both calls run and the deep verdict is final. The
/// news result is persisted for diagnostics but cannot silently disable the second
/// checked pass. In blocking mode the candidate stays off-screen until that final
/// verdict clears the threshold.
async fn score_and_publish(
    deps: &Deps,
    cand: &Candidate,
    intel: &LiveIntel,
    profile: &IssuerProfile,
    opts: &DislocationOptions,
    api_key: &str,
    refresh_deep_intel: bool,
    already_published: bool,
) -> bool {
    let symbol = &cand.alert.symbol;
    let mut published = already_published;
    // Decided by the news pass, then carried unchanged through the deep pass.
    let mut news_so_far: Option<f64> = None;

    'passes: for pass in passes_for(opts.quality_aplus) {
        let pass_name = pass.as_str();
        // A re-score keeps the `Rescoring` label set by the news watch: "the model is
        // reading the headline you can see above" and "first read of a new candidate"
        // are different things to a trader, and `refresh_deep_intel` is already the
        // flag that distinguishes a first score from a re-score.
        if refresh_deep_intel {
            live_status::set_phase(
                symbol,
                match pass {
                    Pass::News => live_status::Phase::News,
                    Pass::Deep => live_status::Phase::Deep,
                },
            );
        }
        log(
            deps,
            "info",
            &format!("dislocation {symbol}: appel {pass_name} démarré"),
        );
        update_event_outcome(
            deps,
            &cand.alert.alert_id,
            if *pass == Pass::Deep {
                "deep_in_progress"
            } else {
                "news_llm_in_progress"
            },
        );
        // The deep pass reads filings and financials, so it gets its own, wider
        // gather. The news pass reuses what we already have.
        let intel_for_pass = if *pass == Pass::Deep {
            let plan = if refresh_deep_intel {
                intel_live::GatherPlan::DEEP
            } else {
                intel_live::GatherPlan::CACHE_ONLY
            };
            let mut deep = intel_live::gather(
                &deps.db,
                &deps.config,
                &deps.secrets,
                symbol,
                intel_live::DEEP_BUDGET,
                plan,
            )
            .await;
            // The deep call refreshed the DB dossier only. Reuse the already-fetched
            // news (and its provenance) instead of paying the same REST sweep twice.
            deep.news = intel.news.clone();
            deep.news_source = intel.news_source;
            deep
        } else {
            intel.clone()
        };

        // The news pass intentionally does not wait for SEC. When the deep refresh reveals
        // a freshly-priced offering that the cache missed, enforce the same stated
        // hard veto here instead of merely hoping the model penalises it.
        if *pass == Pass::Deep {
            let cfg = QualityConfig::DEFAULT;
            if let Err(r) = dilution_gate(
                intel_for_pass
                    .offering_priced_within_hours(cfg.offering_priced_hours, market_now(deps)),
                &cfg,
            ) {
                refuse(
                    deps,
                    opts,
                    cand,
                    symbol,
                    &r,
                    published || opts.alert_before_score,
                );
                break;
            }
        }

        let payload = build_payload(deps, cand, &intel_for_pass, profile);
        let score_budget = match pass {
            Pass::News => NEWS_SCORE_TIMEOUT,
            Pass::Deep => DEEP_SCORE_TIMEOUT,
        };
        let scored = tokio::time::timeout(
            score_budget,
            crate::quality_llm::score(api_key, *pass, symbol, &payload),
        )
        .await;
        let answer = match scored {
            Ok(Ok(a)) => a,
            Ok(Err(e)) => {
                let r = Rejection::ScoringFailed { error: e };
                // A failed call is not evidence that the ticker is A+. Blocking mode
                // therefore fails closed; `alert_before_score` has already displayed
                // an explicitly pending row and keeps that separate contract.
                log(
                    deps,
                    "warn",
                    &format!("dislocation {symbol}: {}", r.reason()),
                );
                update_event_outcome(deps, &cand.alert.alert_id, "a_plus_scoring_failed_closed");
                if opts.alert_before_score {
                    let mut a = cand.alert.clone();
                    a.priority = 4;
                    a.reason = format!("[A+ score indisponible] {}", a.reason);
                    crate::scanner::push_alert(&deps.active_alerts, &deps.alert_history, a);
                }
                if *pass == Pass::News && opts.quality_aplus {
                    continue 'passes;
                }
                break 'passes;
            }
            Err(_) => {
                let r = Rejection::ScoringFailed {
                    error: format!("délai {} s dépassé", score_budget.as_secs()),
                };
                log(
                    deps,
                    "warn",
                    &format!("dislocation {symbol}: {}", r.reason()),
                );
                update_event_outcome(deps, &cand.alert.alert_id, "a_plus_scoring_timeout_closed");
                if opts.alert_before_score {
                    let mut a = cand.alert.clone();
                    a.priority = 4;
                    a.reason = format!("[A+ score indisponible] {}", a.reason);
                    crate::scanner::push_alert(&deps.active_alerts, &deps.alert_history, a);
                }
                if *pass == Pass::News && opts.quality_aplus {
                    continue 'passes;
                }
                break 'passes;
            }
        };

        // The tape of THIS instant, not of the instant the task was spawned. The
        // fallback is the candidate's own snapshot, for the legacy engine and for a
        // symbol the loop has since dropped from its ranking.
        let (rvol, attn) = tape_thirds(symbol).unwrap_or((cand.scores.rvol, cand.scores.attention));
        let v = verdict(
            *pass,
            answer,
            score::Scores {
                rvol,
                attention: attn,
                // The deep pass does NOT re-grade the headline. Its rubric is the v3
                // dossier text: it scores float, dilution, financials and the tape
                // all at once, on anchors where "no news at all" is worth 74. Feeding
                // that number into a third that is supposed to mean "how good is this
                // catalyst" is what took JUNS from 75 to 42 on the 21 August replay.
                // Until the deep rubric is rewritten for this scale, A+ enriches the
                // dossier and never moves the score. ONE EXCEPTION, in `verdict`:
                // when the news pass never answered at all (an API failure), this is
                // None and the deep number does fill the third — some information
                // beats none, and that path only opens on an error.
                news: news_so_far,
            },
            cand.news_age_minutes,
            opts.alert_score_threshold,
        );
        news_so_far = v.scores.news;
        log(
            deps,
            if v.passed() { "info" } else { "info" },
            &v.trace(symbol),
        );
        persist(deps, cand, &v);
        if let Some(third) = v.scores.news {
            record_news_third(symbol, third);
        }
        update_event_outcome(
            deps,
            &cand.alert.alert_id,
            if v.passed() {
                if *pass == Pass::Deep {
                    "deep_pass"
                } else {
                    "news_llm_pass"
                }
            } else if *pass == Pass::Deep {
                "deep_reject"
            } else {
                "news_llm_reject"
            },
        );

        let final_configured_pass = *pass == Pass::Deep || !opts.quality_aplus;
        if final_configured_pass && (v.passed() || published || opts.alert_before_score) {
            let mut a = cand.alert.clone();
            decorate(&mut a, &v);
            publish(deps, opts, a);
            published = published || v.passed();
        }
        if !may_continue_after(*pass, v.passed()) {
            break 'passes;
        }
    }
    published
}

// ─── The thirds, shared between the engine loop and the qualification task ─────
//
// Two registries, one in each direction, because the two halves of the score live
// on opposite sides of an async boundary:
//
//   TAPE_THIRDS  written by the tick, read by the verdict. The qualification task
//                is spawned and then waits seconds for a model. Scoring against the
//                tape as it was WHEN THE TASK STARTED is what erased JUNS on the
//                21 August replay: nominated at 11:08:40 in the pre-ignition lull
//                (RVOL 0,5 · attention 25,7), it was still being judged on those
//                numbers at 11:11:18, by which time it was doing 80x on attention
//                84. The verdict must read the tape of the instant it is rendered.
//
//   NEWS_THIRD   written by the verdict, read by the tick. The mirror case: the two
//                tape thirds keep moving after the model has spoken, so a candidate
//                that landed just under the bar can clear it a minute later with
//                nothing new to buy.
//
// Deliberately not the database: the loop runs every second, and a per-symbol
// SELECT per tick to read numbers we already had in hand would be absurd.
static NEWS_THIRD: std::sync::LazyLock<Mutex<std::collections::HashMap<String, f64>>> =
    std::sync::LazyLock::new(|| Mutex::new(std::collections::HashMap::new()));
static TAPE_THIRDS: std::sync::LazyLock<Mutex<std::collections::HashMap<String, (f64, f64)>>> =
    std::sync::LazyLock::new(|| Mutex::new(std::collections::HashMap::new()));

/// The news third of the last verdict for `symbol`, when there is one.
pub fn news_third(symbol: &str) -> Option<f64> {
    NEWS_THIRD.lock().ok()?.get(symbol).copied()
}

fn record_news_third(symbol: &str, value: f64) {
    if let Ok(mut m) = NEWS_THIRD.lock() {
        m.insert(symbol.to_string(), value);
    }
}

/// Publish this tick's two tape thirds for `symbol`. Called by the engine loop for
/// every ranked candidate, so a verdict rendered seconds later scores the move as it
/// is rather than as it was.
pub fn record_tape(symbol: &str, rvol: f64, attention: f64) {
    if let Ok(mut m) = TAPE_THIRDS.lock() {
        m.insert(symbol.to_string(), (rvol, attention));
    }
}

/// The freshest tape thirds for `symbol`, when the engine has published any.
pub fn tape_thirds(symbol: &str) -> Option<(f64, f64)> {
    TAPE_THIRDS.lock().ok()?.get(symbol).copied()
}

/// Drop every remembered third. Called on a replay reset, where the same symbol is
/// about to be judged again from scratch and yesterday's numbers would be a lie.
pub fn reset_news_thirds() {
    if let Ok(mut m) = NEWS_THIRD.lock() {
        m.clear();
    }
    if let Ok(mut m) = TAPE_THIRDS.lock() {
        m.clear();
    }
}

/// Snapshot the structural facts Q1 reads.
#[allow(dead_code)]
fn read_structure(deps: &Deps, cand: &Candidate, refreshed_float: Option<u64>) -> Structure {
    let symbol = &cand.alert.symbol;
    let now = crate::time::now();
    let (session_dollar_volume, session_vwap) = {
        let ms = deps.market.read().unwrap();
        let t = ms.tickers.get(symbol);
        (
            t.and_then(|t| t.vwap)
                .map(|v| v * t.map(|t| t.volume_day).unwrap_or(0) as f64),
            t.and_then(|t| t.vwap),
        )
    };
    let days_since_split = deps.db.lock().ok().and_then(|conn| {
        crate::local_db::cache_repository::get_dilution_and_splits(&conn, symbol)
            .ok()
            .and_then(|d| d.last_split_date)
            .and_then(|s| chrono::NaiveDate::parse_from_str(&s, "%Y-%m-%d").ok())
            .map(|d| (now.date_naive() - d).num_days())
    });
    Structure {
        price: cand.alert.price.unwrap_or(0.0),
        float_shares: cand.alert.float_shares.or(refreshed_float),
        session_dollar_volume,
        session_vwap,
        days_since_split,
    }
}

/// `pub(crate)`: Micro Pullback's own (lighter, News-only) scoring reuses this rather
/// than re-deriving an issuer profile from `LiveIntel` a second way.
pub(crate) fn issuer_profile(intel: &LiveIntel) -> IssuerProfile {
    match &intel.meta {
        Some(m) => IssuerProfile::from_meta(
            m.country.as_deref(),
            m.industry.as_deref(),
            m.sector.as_deref(),
        ),
        None => IssuerProfile::default(),
    }
}

fn news_count(deps: &Deps, symbol: &str) -> usize {
    let now = market_now(deps);
    deps.market
        .read()
        .unwrap()
        .recent_news_count(symbol, now, 24 * 3600)
}

pub(crate) fn market_now(deps: &Deps) -> DateTime<Utc> {
    deps.market
        .read()
        .unwrap()
        .strategy_market_now()
        .unwrap_or_else(crate::time::now)
}

/// Priority at or above which an alert is worth waking the trader's phone.
///
/// 5 is what the engine assigns to a departure that reached the screen on its own
/// merits. A candidate the deep pass later demoted carries 4, and must NOT buzz: the
/// whole point of the demotion is that it stopped being worth interrupting for.
const PHONE_WORTHY_PRIORITY: u8 = 5;

fn publish(deps: &Deps, opts: &DislocationOptions, alert: AlertSignal) {
    // Before push_alert, so the phone leaves at the same instant the row appears
    // rather than after the desktop notification chain has run.
    if opts.ntfy_alerts && alert.priority >= PHONE_WORTHY_PRIORITY {
        crate::ntfy::notify_alert_once(&deps.secrets, &alert);
    }
    crate::scanner::push_alert(&deps.active_alerts, &deps.alert_history, alert);
}

/// Turn a candidate away. In `alert_before_score` mode a pending row is already on
/// screen, so it is demoted in place rather than left claiming a score that will
/// never come.
fn refuse(
    deps: &Deps,
    opts: &DislocationOptions,
    cand: &Candidate,
    symbol: &str,
    r: &Rejection,
    was_published: bool,
) {
    log(
        deps,
        "info",
        &format!("dislocation {symbol} écarté: {}", r.reason()),
    );
    update_event_outcome(deps, &cand.alert.alert_id, "a_plus_hard_reject");
    if was_published {
        let mut a = cand.alert.clone();
        // Priority 4 is also what keeps the phone quiet: `publish` only pushes to
        // ntfy at PHONE_WORTHY_PRIORITY.
        a.priority = 4;
        a.reason = format!("[A+ écarté · {}] {}", r.reason(), a.reason);
        publish(deps, opts, a);
    }
}

fn update_event_outcome(deps: &Deps, event_id: &str, outcome: &str) {
    if let Ok(conn) = deps.db.lock() {
        let _ =
            crate::local_db::micro_attention_repository::update_outcome(&conn, event_id, outcome);
    }
}

fn log(deps: &Deps, level: &str, msg: &str) {
    if let Ok(conn) = deps.db.lock() {
        let _ = crate::local_db::insert_log(&conn, level, msg);
    }
}

fn persist(deps: &Deps, cand: &Candidate, v: &Verdict) {
    if let Ok(conn) = deps.db.lock() {
        let _ = crate::local_db::quality_repository::upsert(&conn, &cand.alert.symbol, cand, v);
    }
}

/// Assemble the payload. Every field group is filled from `intel` (live or cached)
/// or set to an explicit null with its path recorded in `data_quality.missing` —
/// never dropped. See `quality_llm::payload` for why that matters.
///
/// `pub(crate)`: the payload describes the TICKER's live state, not anything specific
/// to Dislocation's own three-thirds blend, so Micro Pullback's News-only scoring
/// reuses it verbatim instead of re-assembling the same ~20 fields a second way.
pub(crate) fn build_payload(
    deps: &Deps,
    cand: &Candidate,
    intel: &LiveIntel,
    profile: &IssuerProfile,
) -> pl::QualityPayload {
    let symbol = &cand.alert.symbol;
    let now = market_now(deps);
    let mut missing: Vec<String> = Vec::new();

    let (session_high, session_low, session_vwap, prev_close, volume_day, spread_pct) = {
        let ms = deps.market.read().unwrap();
        match ms.tickers.get(symbol) {
            Some(t) => (
                t.high_day,
                t.low_day,
                t.vwap,
                t.previous_close,
                t.volume_day,
                t.spread
                    .zip(t.last_price)
                    .filter(|(_, p)| *p > 0.0)
                    .map(|(s, p)| s / p * 100.0),
            ),
            None => (None, None, None, None, 0, None),
        }
    };
    let last = cand.alert.price.unwrap_or(0.0);

    let fundamental = intel.fundamental.as_ref();
    let si = intel.intel.as_ref().map(|i| &i.short_interest);
    let float_shares = cand
        .alert
        .float_shares
        .or_else(|| fundamental.and_then(|f| f.float_shares).map(|v| v as u64));
    if float_shares.is_none() {
        missing.push("float_and_short.float_shares".into());
    }
    let short_interest = si.and_then(|s| s.short_interest).map(|v| v as f64);
    if short_interest.is_none() {
        missing.push("float_and_short.short_interest".into());
    }

    let dil = intel.dilution_splits.as_ref();
    let risk = intel.risk.as_ref();
    let fin = intel.intel.as_ref().map(|i| &i.financials);
    let dilution = intel.intel.as_ref().map(|i| &i.dilution);

    // News: the REST sweep plus whatever the websocket holds, newest first.
    let news = build_news(deps, cand, intel, &mut missing);

    pl::QualityPayload {
        identity: pl::Identity {
            symbol: symbol.clone(),
            name: None,
            exchange: None,
            country: intel.meta.as_ref().and_then(|m| m.country.clone()),
            country_risk: profile.country_risk,
            sector: intel.meta.as_ref().and_then(|m| m.sector.clone()),
            industry: intel.meta.as_ref().and_then(|m| m.industry.clone()),
            sic: intel.meta.as_ref().and_then(|m| m.sic.clone()),
            sector_hot: profile.sector_hot,
            sector_bucket: profile.sector_bucket.map(str::to_string),
        },
        trigger: pl::Trigger {
            tempo_secs: cand.tempo_secs,
            tempo_depth: cand.tempo_depth.clone(),
            price_return_pct: cand.price_return_pct,
            volume_ratio: cand.volume_ratio,
            trade_rate_ratio: cand.trade_rate_ratio,
            range_ratio: cand.range_ratio,
            detected_at: cand.detected_at.to_rfc3339(),
        },
        tape: pl::Tape {
            source: match cand.tape_source {
                TapeKind::Prints => "prints".into(),
                TapeKind::SecondAggregates => "second_aggregates".into(),
            },
            rate_per_sec: cand.tape_rate_per_sec,
            dollar_per_sec: cand.tape_dollar_per_sec,
            active_seconds: cand.tape_active_seconds,
            window_secs: cand.tape_window_secs,
        },
        price: pl::Price {
            last,
            session_high,
            session_low,
            session_vwap,
            distance_to_vwap_pct: session_vwap
                .filter(|v| *v > 0.0)
                .map(|v| (last - v) / v * 100.0),
            prev_close,
            change_day_pct: cand.alert.change_day_pct,
            spread_pct,
        },
        volume: pl::Volume {
            session_volume: volume_day,
            session_dollar_volume: session_vwap.map(|v| v * volume_day as f64),
            rvol: cand.alert.rvol,
            avg_daily_volume: fundamental.and_then(|f| f.avg_volume).map(|v| v as f64),
        },
        float_and_short: pl::FloatAndShort {
            float_shares,
            shares_outstanding: fundamental
                .and_then(|f| f.outstanding_shares)
                .map(|v| v as u64),
            free_float_pct: fundamental.and_then(|f| f.free_float),
            market_cap: None,
            short_interest,
            short_interest_pct_float: short_interest
                .zip(float_shares.filter(|f| *f > 0))
                .map(|(si, f)| si / f as f64 * 100.0),
            days_to_cover: si.and_then(|s| s.days_to_cover),
            shortable: None,
        },
        news,
        risk: pl::Risk {
            dilution_score: risk.and_then(|r| r.dilution_score),
            dilution_capacity: risk.and_then(|r| r.dilution_capacity_score),
            dilution_need: risk.and_then(|r| r.dilution_need_score),
            dilution_pct_12m: dil.and_then(|d| d.dilution_pct_12m),
            pump_dump_score: risk.and_then(|r| r.pump_dump_score),
            has_recent_shelf: dilution.map(|d| d.has_recent_shelf),
            latest_dilution_form: dilution.and_then(|d| d.latest_form.clone()),
            latest_dilution_date: dilution.and_then(|d| d.latest_date.clone()),
            atm_active: intel.atm_active(),
        },
        sec_filings: intel
            .filings
            .iter()
            .map(|f| pl::Filing {
                form_type: Some(f.form_type.clone()),
                date: f.filing_date.clone(),
                category: Some(f.category.clone()),
                atm: Some(f.detected_atm),
                resale: Some(f.detected_resale),
                warrants: Some(f.detected_warrants),
                offering_amount: f.offering_amount,
            })
            .collect(),
        financials: pl::Financials {
            net_income_last_q: fin.and_then(|f| f.net_income_last_q),
            net_income_ttm: fin.and_then(|f| f.net_income_ttm),
            negative_quarters_last4: fin.and_then(|f| f.negative_quarters_last4),
            operating_cash_flow_ttm: fin.and_then(|f| f.operating_cash_flow_ttm),
            cash_and_equivalents: fin.and_then(|f| f.cash_and_equivalents),
            period_end: fin.and_then(|f| f.period_end.clone()),
            cash_runway_quarters: intel.cash_runway_quarters(),
        },
        splits: pl::Splits {
            last_split_date: dil.and_then(|d| d.last_split_date.clone()),
            split_label: dil.and_then(|d| d.last_split_label.clone()),
            days_since_split: dil
                .and_then(|d| d.last_split_date.as_deref())
                .and_then(|s| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
                .map(|d| (now.date_naive() - d).num_days()),
            splits_last_1y: dil.and_then(|d| d.split_count_1y),
        },
        data_quality: pl::DataQuality::new(
            &feed_label(deps),
            &[
                ("float_and_short", intel.float_and_short_source),
                ("news", intel.news_source),
                ("risk", intel.risk_source),
                ("sec_filings", intel.filings_source),
                ("financials", intel.financials_source),
            ],
            missing,
        ),
        context: pl::Context {
            mode: if crate::replay::clock::is_active() {
                "replay".into()
            } else {
                "live".into()
            },
            as_of: now.to_rfc3339(),
            trading_day: crate::time::et_date(now),
            session: session_label(now),
            // 09:30 ET is minute 570.
            minutes_to_open: 570 - crate::time::et_minutes(now) as i64,
        },
    }
}

/// The news list, REST sweep first then anything the websocket holds that the sweep
/// missed. Ages are computed against `now` and against the departure — the second is
/// what tells the model whether a headline could have caused the move at all.
fn build_news(
    deps: &Deps,
    cand: &Candidate,
    intel: &LiveIntel,
    missing: &mut Vec<String>,
) -> Vec<pl::NewsItem> {
    let now = market_now(deps);
    let mut out: Vec<pl::NewsItem> = Vec::new();
    let mut seen: Vec<String> = Vec::new();

    for a in intel.news.iter().filter(|a| a.created_at <= now) {
        let published = a.created_at;
        seen.push(a.headline.to_lowercase());
        out.push(pl::NewsItem {
            published_at: published.to_rfc3339(),
            age_minutes: (now - published).num_minutes(),
            minutes_vs_move: (published - cand.detected_at).num_minutes(),
            headline: a.headline.clone(),
            source: Some(a.source.clone()).filter(|s| !s.is_empty()),
            body_excerpt: excerpt(&a.content),
        });
    }

    for n in deps
        .market
        .read()
        .unwrap()
        .recent_news(&cand.alert.symbol, now, 24 * 3600)
    {
        if seen.iter().any(|h| *h == n.headline.to_lowercase()) {
            continue;
        }
        // `NewsRef.at` is when the headline reached US, not when the wire published
        // it — the websocket carries no publication timestamp. Close enough while the
        // feed is connected, and the REST sweep above supplies the real one whenever
        // both saw the same story.
        out.push(pl::NewsItem {
            published_at: n.at.to_rfc3339(),
            age_minutes: (now - n.at).num_minutes(),
            minutes_vs_move: (n.at - cand.detected_at).num_minutes(),
            headline: n.headline.clone(),
            source: None,
            body_excerpt: None,
        });
    }

    out.sort_by_key(|n| n.age_minutes);
    if out.is_empty() {
        missing.push("news".into());
    }
    out
}

/// Trim an article body to something a fast model can read without paying for the
/// whole press release.
fn excerpt(body: &str) -> Option<String> {
    let clean = body.trim();
    if clean.is_empty() {
        return None;
    }
    Some(clean.chars().take(700).collect())
}

/// The session name in the SAME spelling the rubric and the DB use.
///
/// NOT `format!("{:?}")`: that renders `PreOpen` as "preopen", while the rubric's
/// open-exception rule keys on "pre_open". Going through serde uses the enum's own
/// `rename_all = "snake_case"`, so one spelling is defined in one place.
pub fn session_label(now: DateTime<Utc>) -> String {
    serde_json::to_value(crate::time::session_at(now))
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| "inconnu".to_string())
}

fn feed_label(deps: &Deps) -> String {
    let ms = deps.market.read().unwrap();
    match ms.tape_kind() {
        TapeKind::SecondAggregates => "massive_delayed".to_string(),
        TapeKind::Prints => {
            let f = ms.feed.feed.to_lowercase();
            if f.contains("sip") {
                "sip".to_string()
            } else if f.contains("iex") {
                "iex".to_string()
            } else {
                "inconnu".to_string()
            }
        }
    }
}

#[cfg(test)]
mod label_tests {
    use super::*;
    use chrono::TimeZone;

    /// `format!("{:?}")` renders `PreOpen` as "preopen", which is NOT the spelling the
    /// rubric's open-exception rule keys on, nor the one the DB column carries.
    #[test]
    fn session_labels_are_snake_case_everywhere() {
        // 13:35 UTC = 09:35 ET in June (EDT) → pre-open.
        assert_eq!(
            session_label(Utc.with_ymd_and_hms(2026, 6, 8, 13, 35, 0).unwrap()),
            "pre_open"
        );
        // 08:30 UTC = 04:30 ET → premarket.
        assert_eq!(
            session_label(Utc.with_ymd_and_hms(2026, 6, 8, 8, 30, 0).unwrap()),
            "premarket"
        );
        // 15:00 UTC = 11:00 ET → open.
        assert_eq!(
            session_label(Utc.with_ymd_and_hms(2026, 6, 8, 15, 0, 0).unwrap()),
            "open"
        );
    }

    /// The news rubric no longer branches on the session: everything it is shown is
    /// at most twenty minutes old, so the "open exception" it used to carry — an
    /// overnight headline legitimately explaining a 09:30 gap — has nothing left to
    /// except. The label still travels in the payload as context, and this pins the
    /// fact that dropping the branch was deliberate.
    #[test]
    fn the_news_rubric_no_longer_needs_a_session_exception() {
        let rubric = crate::quality_llm::prompt::SYSTEM_NEWS;
        assert!(!rubric.contains("Exception d'ouverture"));
        assert!(rubric.contains("`age_minutes` est traité hors de ta note"));
    }
}
