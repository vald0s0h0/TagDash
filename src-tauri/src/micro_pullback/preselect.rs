// The gate between "the tape moved" and "spend a model call on it".
//
// v2's ranking answers a market-microstructure question: is attention concentrating
// on this ticker right now? That question has nothing to say about WHY, and A+ is a
// catalyst decision. Everything here is the deterministic bridge between the two.
//
// ── RANK, DON'T THRESHOLD ─────────────────────────────────────────────────────
// An earlier version ANDed together three magnitude gates: RVOL >= 5, flow in the
// top 20 %, attention >= 1 robust sigma. Measured over the 44 391 attention rows of
// the 21 August replay, that combination is not merely brittle — it is
// anti-correlated with market activity:
//
//   * `RVOL >= 5` rejects **100 %** of candidates through the whole cold premarket
//     (04:00-06:00 ET) and through the first ~15 minutes of any session or run: the
//     rolling baseline needs prior windows inside the 15 minutes of retained tape,
//     so before that the ratio does not exist — and an absent value failed a `>=`
//     test exactly as surely as a bad one;
//   * at 10:00 ET, the busiest hour measured (392 symbols, mean z 1.81, only 5.6 %
//     weak flow), that same gate saw nothing at all;
//   * `flow_rank >= 0.80` is a percentile, so "top 20 %" of ten active names and of
//     eight hundred are unrelated objects — it bites hardest when the tape is
//     richest.
//
// So only PRECONDITIONS are gates here: things that make the question meaningless
// rather than merely weak. Everything else feeds one composite `priority`, built
// from percentiles of the CURRENT tick and therefore comparable between a cold
// 04:00 tape and a hot 09:35 one. The engine then spends a bounded budget on the
// best few (see `v2::start`) — which is also what the trader observes: the name
// worth taking is usually the top of the list, not the one that cleared some bar.
//
// An unmeasurable RVOL is UNRANKED, not rejected: its weight is redistributed over
// the terms that do exist, the same way `attention` already handles a missing
// relative rank. "We could not measure it" and "it is the market's worst" are
// opposite statements and must not produce the same number.
//
// ── WHY A GATE AT ALL ─────────────────────────────────────────────────────────
//   1. THE TRIAGE PASS ONLY READS NEWS. Handing it a candidate with no fresh
//      headline asks it to grade an absence — which it will do, on the setup, and
//      that is exactly how a purely technical departure used to outscore a real
//      catalyst. No fresh headline, no call.
//   2. THE ISSUER IS KNOWN BEFORE THE MODEL SPEAKS. Hot industry, risk domicile and
//      a recent reverse split come from the database, already in memory. They belong
//      in deterministic terms the trader can audit, not inside a number the model
//      made up.
//
// PURE ON PURPOSE. Everything below is a function over plain data — no market, no
// database, no clock. That is what makes it testable, and what lets the refusal
// reasons be written to the forensic table instead of being lost.
//
// The points do NOT decide anything by themselves: they are applied to the model's
// Score Breaking afterwards (see `quality_llm::adjust`), never before. A candidate
// that reaches the model still fails on a weak catalyst, and a hot-industry bonus
// cannot manufacture a catalyst that isn't there.

use crate::strategies::issuer_profile::IssuerProfile;
use crate::types::Session;

// ─── The one magnitude-free gate ───────────────────────────────────────────────

/// How fresh the newest headline must be for a candidate to be worth a model call.
///
/// Twenty minutes, in every session. Deliberately stricter than the rubric's own
/// "fraiche" band: the A+ layer exists to catch the moment an information hits the
/// tape, and a headline the market has had twenty minutes to read is no longer
/// explaining an acceleration that starts now.
///
/// THE COST, STATED: there is no open exception. A name gapping at 09:30 on a wire
/// published at 06:00 will not be scored — at 09:30 that headline is 210 minutes
/// old. That is a real class of runner given up on purpose.
pub const FRESH_NEWS_MAX_MINUTES: i64 = 20;

// ─── Priority weights ──────────────────────────────────────────────────────────
// Every term is a 0..1 percentile of the current tick, so the composite needs no
// normalisation against a historical distribution — that is the whole point.

/// Cross-sectional attention: flow, ATR-normalised range, continuity, distribution.
pub const W_ATTENTION: f64 = 0.40;
/// Rank of the ticker's own tempo RVOL within the tick. The trader's own read is
/// "attention plus RVOL"; these two weights say so.
pub const W_RVOL_RANK: f64 = 0.30;
/// How much of the freshness window is left. Inside a 20-minute gate, a 2-minute
/// headline and an 18-minute one are not the same catalyst.
pub const W_NEWS: f64 = 0.20;
/// Tempo depth: a move that survived being measured at several time scales.
pub const W_DEPTH: f64 = 0.10;
/// Tempos the engine can run, for the depth percentile.
const MAX_TEMPOS: f64 = 6.0;

// ─── Deterministic points, applied to the model's Score Breaking ───────────────

/// An industry that structurally pulls a premarket crowd. The same catalyst buys
/// materially more participation in biotech or space than in an industrial supplier,
/// and that difference is observable before any model speaks.
pub const BONUS_HOT_INDUSTRY: f64 = 15.0;

/// Domiciles heavily over-represented in US small-cap pump & dump schemes. A base
/// rate, never a hard reject unless the user asks for one (`exclude_risk_domicile`):
/// a genuinely strong catalyst still clears the bar, it just has to be genuinely
/// strong.
pub const PENALTY_RISK_DOMICILE: f64 = -25.0;

/// Tempo RVOL at which participation stops being "elevated" and starts being the
/// kind of number that only appears on names that run.
///
/// Percentiles of the real distribution, not round numbers. Tempo RVOL is violently
/// right-tailed — the robust baseline of a dormant premarket name is near zero, so
/// the ratio routinely reaches four and five figures. Among the rows that cleared
/// the old gates on 21 August the median is 51x and the 70th percentile 194x: a
/// "20x is exceptional" bonus would have fired on two thirds of the population and
/// meant nothing.
pub const RVOL_HIGH: f64 = 200.0;
pub const BONUS_RVOL_HIGH: f64 = 5.0;
/// ~90th percentile of the same population (3 667x). JUNS's ignition measured
/// 4 378x on the 20 s tempo.
pub const RVOL_EXTREME: f64 = 3_500.0;
pub const BONUS_RVOL_EXTREME: f64 = 10.0;

/// Attention so detached that the name is not merely in the top decile, it is the
/// story of the minute: ~3 % of the rows that cleared the old gates. JUNS's ignition
/// measured 3.06-3.28.
pub const ATTENTION_Z_EXTREME: f64 = 3.0;
pub const BONUS_ATTENTION_EXTREME: f64 = 5.0;

/// Three or more tempos agreeing is a different object from a 10-second spike.
pub const TEMPO_DEPTH_DEEP: usize = 3;
pub const BONUS_TEMPO_DEPTH: f64 = 5.0;

/// A reverse split this recent means the float and the price history are both
/// artefacts, and the pattern is overwhelmingly a failing issuer selling into
/// strength.
///
/// THIS LIVES HERE, NOT IN THE RUBRIC. The triage prompt used to carry a hard cap
/// ("offering, or reverse split under 5 days -> 20 max"). That is what crushed JUNS
/// to 20 on 21 August: its split was FOURTEEN days old, outside the stated window,
/// but a model reading "reverse split 1:75 il y a 14 jours" in its own `red_flags`
/// applied the cap anyway. The cap also contradicted the rubric's own instruction
/// not to grade the structure, and double-counted a fact these terms already own. A
/// number applied here is auditable, tunable without touching a prompt, and cannot
/// be misread.
pub const RECENT_SPLIT_DAYS: i64 = 30;
pub const PENALTY_RECENT_SPLIT: f64 = -15.0;

// ─── Input / output ────────────────────────────────────────────────────────────

/// One candidate, as the gate needs to see it. Assembled by the engine from the tick
/// it already has in hand — every rank below is already computed by `rank_all`.
#[derive(Debug, Clone, Copy)]
pub struct Candidate {
    /// Cross-sectional attention, 0..100.
    pub attention: f64,
    /// Percentile of this ticker's tempo RVOL within the tick, 0..1. `None` when no
    /// baseline exists yet — unranked, never zero.
    pub rvol_rank: Option<f64>,
    /// Raw robust tempo RVOL, for the excellence tiers. `None` = not measurable.
    pub tempo_rvol: Option<f64>,
    /// Attention distance from the market median this tick, in robust sigmas.
    pub attention_z: f64,
    /// How many tempos confirmed.
    pub tempo_depth: usize,
    pub profile: IssuerProfile,
    /// Age of the newest headline for this ticker, in minutes. `None` = no headline.
    pub newest_news_age_minutes: Option<i64>,
    /// Days since the last split, when known.
    pub days_since_split: Option<i64>,
    pub session: Session,
}

/// Why a candidate was not worth a model call. Typed rather than a bool, for the
/// same reason `quality::Rejection` is: a candidate that never reached the screen is
/// invisible, and tuning afterwards requires knowing what turned it away.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Refusal {
    /// `hot_industry_only` is on and this issuer's industry is not in a hot
    /// category — including because we have never collected it.
    NotHotIndustry,
    /// `exclude_risk_domicile` is on and this issuer is domiciled in one of the four.
    RiskDomicile,
    NoFreshNews {
        age_minutes: Option<i64>,
    },
}

impl Refusal {
    /// One line for `local_logs`.
    pub fn reason(&self) -> String {
        match self {
            Refusal::NotHotIndustry => "industrie hors catégories porteuses".into(),
            Refusal::RiskDomicile => "juridiction à risque exclue".into(),
            Refusal::NoFreshNews { age_minutes: None } => "aucune news".into(),
            Refusal::NoFreshNews {
                age_minutes: Some(m),
            } if *m < 0 => format!("news horodatée dans le futur ({m} min)"),
            Refusal::NoFreshNews {
                age_minutes: Some(m),
            } => format!("news vieille de {m} min (max {FRESH_NEWS_MAX_MINUTES})"),
        }
    }

    /// The outcome string written to `micro_attention_events`, so the funnel stays
    /// countable per reason after the fact.
    pub fn outcome(&self) -> &'static str {
        match self {
            Refusal::NotHotIndustry => "preselect_not_hot_industry",
            Refusal::RiskDomicile => "preselect_risk_domicile",
            Refusal::NoFreshNews { .. } => "preselect_no_fresh_news",
        }
    }
}

/// Every deterministic term that will move the model's Score Breaking. Kept
/// separately so a candidate's final number can be explained without re-running
/// anything.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SetupTerms {
    pub hot_industry: f64,
    pub risk_domicile: f64,
    pub exceptional_rvol: f64,
    pub market_attention: f64,
    pub tempo_depth: f64,
    pub recent_split: f64,
}

impl SetupTerms {
    pub fn total(&self) -> f64 {
        self.hot_industry
            + self.risk_domicile
            + self.exceptional_rvol
            + self.market_attention
            + self.tempo_depth
            + self.recent_split
    }
}

/// A candidate that cleared the preconditions, with the order it deserves.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Selection {
    pub setup: SetupTerms,
    /// Where this candidate sits among the tick's others. Composite of the tick's own
    /// percentiles plus the deterministic points, so it is comparable between a cold
    /// 04:00 tape and a hot 09:35 one. Higher is better; the engine spends its
    /// budget from the top.
    pub priority: f64,
}

/// The switches that turn a scoring term into a hard exclusion.
#[derive(Debug, Clone, Copy, Default)]
pub struct Filters {
    pub hot_industry_only: bool,
    pub exclude_risk_domicile: bool,
}

/// Whether a headline of `age_minutes` can still explain a move starting now.
///
/// The same ceiling in every session, on purpose. A negative age means a headline
/// stamped in the future — a provider clock skew, never a catalyst — and is refused
/// rather than treated as maximally fresh.
pub fn news_is_fresh_enough(age_minutes: i64, _session: Session) -> bool {
    (0..=FRESH_NEWS_MAX_MINUTES).contains(&age_minutes)
}

/// How much of the freshness window is left, 1.0 (just published) to 0.0 (at the
/// ceiling). Only meaningful for an age that already passed the gate.
fn news_term(age_minutes: i64) -> f64 {
    let left = (FRESH_NEWS_MAX_MINUTES - age_minutes.max(0)) as f64;
    (left / FRESH_NEWS_MAX_MINUTES as f64).clamp(0.0, 1.0)
}

/// Run the preconditions and, when they pass, compute the order and the points.
pub fn preselect(c: &Candidate, f: &Filters) -> Result<Selection, Refusal> {
    // Cheapest and most absolute first, so the logged reason is the one the user can
    // act on rather than an incidental second failure.
    if f.hot_industry_only && !c.profile.sector_hot {
        return Err(Refusal::NotHotIndustry);
    }
    if f.exclude_risk_domicile && c.profile.country_risk {
        return Err(Refusal::RiskDomicile);
    }
    let age = match c.newest_news_age_minutes {
        Some(age) if news_is_fresh_enough(age, c.session) => age,
        age => return Err(Refusal::NoFreshNews { age_minutes: age }),
    };

    let setup = SetupTerms {
        hot_industry: if c.profile.sector_hot {
            BONUS_HOT_INDUSTRY
        } else {
            0.0
        },
        risk_domicile: if c.profile.country_risk {
            PENALTY_RISK_DOMICILE
        } else {
            0.0
        },
        // Tiered, not cumulative: an extreme RVOL is not "high plus extra".
        exceptional_rvol: match c.tempo_rvol {
            Some(r) if r >= RVOL_EXTREME => BONUS_RVOL_EXTREME,
            Some(r) if r >= RVOL_HIGH => BONUS_RVOL_HIGH,
            _ => 0.0,
        },
        market_attention: if c.attention_z >= ATTENTION_Z_EXTREME {
            BONUS_ATTENTION_EXTREME
        } else {
            0.0
        },
        tempo_depth: if c.tempo_depth >= TEMPO_DEPTH_DEEP {
            BONUS_TEMPO_DEPTH
        } else {
            0.0
        },
        // An unknown split date is NOT a recent split: we never penalise a ticker
        // for a gap in our own data.
        recent_split: match c.days_since_split {
            Some(d) if (0..=RECENT_SPLIT_DAYS).contains(&d) => PENALTY_RECENT_SPLIT,
            _ => 0.0,
        },
    };

    // Weighted mean over the terms that EXIST. A missing rvol rank redistributes its
    // weight rather than scoring zero.
    let mut num = W_ATTENTION * (c.attention / 100.0).clamp(0.0, 1.0)
        + W_NEWS * news_term(age)
        + W_DEPTH * (c.tempo_depth as f64 / MAX_TEMPOS).clamp(0.0, 1.0);
    let mut den = W_ATTENTION + W_NEWS + W_DEPTH;
    if let Some(rank) = c.rvol_rank {
        num += W_RVOL_RANK * rank.clamp(0.0, 1.0);
        den += W_RVOL_RANK;
    }

    Ok(Selection {
        setup,
        priority: 100.0 * num / den + setup.total(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hot() -> IssuerProfile {
        IssuerProfile {
            sector_hot: true,
            sector_bucket: Some("biotech"),
            country_risk: false,
        }
    }

    fn base() -> Candidate {
        Candidate {
            attention: 75.0,
            rvol_rank: Some(0.6),
            tempo_rvol: Some(12.0),
            attention_z: 1.5,
            tempo_depth: 2,
            profile: IssuerProfile::default(),
            newest_news_age_minutes: Some(4),
            days_since_split: None,
            session: Session::Premarket,
        }
    }

    const OPEN: Filters = Filters {
        hot_industry_only: false,
        exclude_risk_domicile: false,
    };

    #[test]
    fn a_candidate_without_a_fresh_headline_never_reaches_the_model() {
        // The whole point: the triage pass grades the catalyst, so no catalyst is
        // not a low score, it is not a question.
        let none = Candidate {
            newest_news_age_minutes: None,
            ..base()
        };
        assert_eq!(
            preselect(&none, &OPEN),
            Err(Refusal::NoFreshNews { age_minutes: None })
        );
    }

    #[test]
    fn twenty_minutes_is_the_ceiling_in_every_session() {
        let aged = |m: i64, session| Candidate {
            newest_news_age_minutes: Some(m),
            session,
            ..base()
        };
        for session in [
            Session::Premarket,
            Session::PreOpen,
            Session::Open,
            Session::Afterhours,
        ] {
            assert!(preselect(&aged(19, session), &OPEN).is_ok(), "{session:?}");
            assert!(preselect(&aged(21, session), &OPEN).is_err(), "{session:?}");
        }
        // The overnight wire is deliberately given up on: no session buys it back.
        assert!(preselect(&aged(8 * 60, Session::Open), &OPEN).is_err());
    }

    /// A headline stamped in the future is a provider clock skew, not a scoop. It
    /// must not read as "zero minutes old", which is maximal freshness.
    #[test]
    fn a_headline_from_the_future_is_refused() {
        let skewed = Candidate {
            newest_news_age_minutes: Some(-3),
            ..base()
        };
        assert!(matches!(
            preselect(&skewed, &OPEN),
            Err(Refusal::NoFreshNews { .. })
        ));
    }

    /// THE regression this rewrite exists for. An unmeasurable RVOL is 100 % of the
    /// cold premarket and of the first quarter-hour of any run. Under the old
    /// magnitude gate it was a rejection; it must now merely be unranked.
    #[test]
    fn an_unmeasurable_rvol_is_unranked_not_rejected() {
        let cold = Candidate {
            rvol_rank: None,
            tempo_rvol: None,
            ..base()
        };
        let sel = preselect(&cold, &OPEN).expect("04:00 must not be invisible");
        // Its weight is redistributed, so the priority is the mean of the remaining
        // terms — not the same candidate penalised by a zero.
        let with_zero_rank = preselect(
            &Candidate {
                rvol_rank: Some(0.0),
                ..cold
            },
            &OPEN,
        )
        .unwrap();
        assert!(
            sel.priority > with_zero_rank.priority,
            "unranked {} must beat worst-ranked {}",
            sel.priority,
            with_zero_rank.priority
        );
        assert_eq!(sel.setup.exceptional_rvol, 0.0, "no tier without a value");
    }

    /// The trader's own read: attention plus RVOL decide the order.
    #[test]
    fn attention_and_rvol_drive_the_order() {
        let low = preselect(&base(), &OPEN).unwrap().priority;
        let more_attention = preselect(
            &Candidate {
                attention: 95.0,
                ..base()
            },
            &OPEN,
        )
        .unwrap()
        .priority;
        let more_rvol = preselect(
            &Candidate {
                rvol_rank: Some(1.0),
                ..base()
            },
            &OPEN,
        )
        .unwrap()
        .priority;
        assert!(more_attention > low);
        assert!(more_rvol > low);
    }

    /// Freshness ranks INSIDE the window, it does not merely open it.
    #[test]
    fn a_two_minute_headline_outranks_an_eighteen_minute_one() {
        let fresh = preselect(
            &Candidate {
                newest_news_age_minutes: Some(2),
                ..base()
            },
            &OPEN,
        )
        .unwrap();
        let stale = preselect(
            &Candidate {
                newest_news_age_minutes: Some(18),
                ..base()
            },
            &OPEN,
        )
        .unwrap();
        assert!(fresh.priority > stale.priority);
    }

    /// The whole reason this is a rank and not a threshold: a cold 04:00 candidate
    /// and a hot 09:35 one are scored on the same 0..100, because every term is a
    /// percentile of its OWN tick.
    #[test]
    fn cold_and_hot_candidates_land_on_one_comparable_scale() {
        // 04:00: no baseline anywhere, so no RVOL rank for anyone.
        let cold = preselect(
            &Candidate {
                attention: 88.0,
                rvol_rank: None,
                tempo_rvol: None,
                ..base()
            },
            &OPEN,
        )
        .unwrap();
        // 09:35: same relative standing, in a market with eight hundred active names.
        let hot = preselect(
            &Candidate {
                attention: 88.0,
                rvol_rank: Some(0.88),
                tempo_rvol: Some(300.0),
                ..base()
            },
            &OPEN,
        )
        .unwrap();
        assert!(
            (cold.priority - (hot.priority - hot.setup.total())).abs() < 6.0,
            "cold {} vs hot {} must be the same order of magnitude",
            cold.priority,
            hot.priority
        );
    }

    #[test]
    fn the_filters_are_hard_exclusions_when_switched_on() {
        let unknown_industry = base();
        assert!(preselect(&unknown_industry, &OPEN).is_ok());
        assert_eq!(
            preselect(
                &unknown_industry,
                &Filters {
                    hot_industry_only: true,
                    ..OPEN
                }
            ),
            Err(Refusal::NotHotIndustry)
        );

        let risky = Candidate {
            profile: IssuerProfile {
                country_risk: true,
                ..hot()
            },
            ..base()
        };
        // Off: still watched, still penalised.
        assert_eq!(
            preselect(&risky, &OPEN).unwrap().setup.risk_domicile,
            PENALTY_RISK_DOMICILE
        );
        assert_eq!(
            preselect(
                &risky,
                &Filters {
                    exclude_risk_domicile: true,
                    ..OPEN
                }
            ),
            Err(Refusal::RiskDomicile)
        );
    }

    #[test]
    fn the_issuer_terms_dominate_the_setup_points() {
        let terms = preselect(
            &Candidate {
                profile: IssuerProfile {
                    country_risk: true,
                    ..hot()
                },
                ..base()
            },
            &OPEN,
        )
        .unwrap()
        .setup;
        // A hot Cayman shell is still net negative: the domicile outweighs the
        // sector, which is the intended base rate.
        assert_eq!(terms.hot_industry + terms.risk_domicile, -10.0);
    }

    /// The reverse-split penalty moved OUT of the rubric and into a number. An
    /// unknown split date must never behave like a recent one.
    #[test]
    fn a_recent_reverse_split_is_a_deterministic_penalty_not_a_rubric_cap() {
        let with = |d: Option<i64>| {
            preselect(
                &Candidate {
                    days_since_split: d,
                    ..base()
                },
                &OPEN,
            )
            .unwrap()
            .setup
            .recent_split
        };
        assert_eq!(with(Some(14)), PENALTY_RECENT_SPLIT, "JUNS's actual split");
        assert_eq!(with(Some(0)), PENALTY_RECENT_SPLIT);
        assert_eq!(with(Some(31)), 0.0);
        assert_eq!(with(None), 0.0, "unknown is not recent");
    }

    #[test]
    fn excellence_bonuses_are_tiered_not_cumulative() {
        let mk = |rvol: f64, z: f64, depth: usize| {
            preselect(
                &Candidate {
                    tempo_rvol: Some(rvol),
                    attention_z: z,
                    tempo_depth: depth,
                    ..base()
                },
                &OPEN,
            )
            .unwrap()
            .setup
        };
        assert_eq!(mk(12.0, 1.5, 2).total(), 0.0, "an ordinary candidate earns nothing");
        assert_eq!(mk(250.0, 1.5, 2).exceptional_rvol, 5.0);
        assert_eq!(
            mk(4_000.0, 1.5, 2).exceptional_rvol,
            10.0,
            "extreme replaces high, it does not add to it"
        );
        assert_eq!(mk(12.0, 3.5, 2).market_attention, 5.0);
        assert_eq!(mk(12.0, 1.5, 4).tempo_depth, 5.0);
        assert_eq!(mk(4_000.0, 3.5, 4).total(), 20.0);
    }

    /// JUNS's real ignition on 21 August 2026 at 07:01:40 ET. Under the old gates it
    /// squeaked through on a 1.26 sigma; it must now simply rank well.
    #[test]
    fn the_juns_ignition_ranks_near_the_top() {
        let juns = Candidate {
            attention: 80.9,
            rvol_rank: Some(0.95),
            tempo_rvol: Some(1549.0),
            attention_z: 1.26,
            tempo_depth: 2,
            profile: hot(),
            newest_news_age_minutes: Some(2),
            days_since_split: Some(14),
            session: Session::Premarket,
        };
        let sel = preselect(&juns, &OPEN).expect("JUNS must not be gated out");
        assert_eq!(sel.setup.hot_industry, BONUS_HOT_INDUSTRY);
        assert_eq!(sel.setup.exceptional_rvol, BONUS_RVOL_HIGH);
        assert_eq!(sel.setup.recent_split, PENALTY_RECENT_SPLIT);
        // Comfortably above an ordinary candidate, which is all a rank has to do.
        assert!(sel.priority > preselect(&base(), &OPEN).unwrap().priority);
    }

    /// The counter-example from the same session: SNSC, newest headline 46 hours
    /// old. It passed the old pipeline with 72 and was the only name of the day to
    /// reach the screen. The news precondition — not a magnitude — is what stops it.
    #[test]
    fn the_snsc_non_catalyst_never_reaches_the_model() {
        let snsc = Candidate {
            attention: 74.1,
            rvol_rank: Some(0.7),
            tempo_rvol: Some(20.4),
            attention_z: 1.03,
            tempo_depth: 4,
            profile: IssuerProfile::default(),
            newest_news_age_minutes: Some(46 * 60),
            days_since_split: None,
            session: Session::Premarket,
        };
        assert!(matches!(
            preselect(&snsc, &OPEN),
            Err(Refusal::NoFreshNews { .. })
        ));
    }

    #[test]
    fn the_refusal_names_itself_for_the_log_and_the_funnel() {
        let all = [
            Refusal::NotHotIndustry,
            Refusal::RiskDomicile,
            Refusal::NoFreshNews { age_minutes: None },
        ];
        let outcomes: Vec<&str> = all.iter().map(|r| r.outcome()).collect();
        assert_eq!(
            outcomes.len(),
            outcomes
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len(),
            "two refusals sharing one outcome string are uncountable"
        );
        for r in all {
            assert!(!r.reason().is_empty());
            assert!(r.outcome().starts_with("preselect_"));
        }
    }
}
