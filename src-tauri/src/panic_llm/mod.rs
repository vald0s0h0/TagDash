// Panic Score V1 — the LLM ranking of the pre-open watchlist.
//
// BUTTON-TRIGGERED ONLY. Nothing in this module is ever scheduled. The 09:25 ET
// trigger in `crate::panic_watchlist` still promotes the QUANTITATIVE watchlist from
// provisional to final; this layer answers the different question the quant score
// cannot: is the move justified, or is it a flow that will normalise?
//
// The pipeline, per run:
//
//   1. COLLECT — one standardized payload per ticker (`payload.rs`), assembled from
//      stores the Startup Pipeline already fills, plus the news cascade below.
//   2. SCORE — one call per ticker against a frozen absolute rubric (`prompt.rs`),
//      bounded concurrency. Per-ticker isolation is what makes the scores comparable:
//      a batched call lets the model spread notes across whichever peers share the
//      batch, so the same ticker would score differently depending on its batch mates.
//   3. CALIBRATE — one pass over the (symbol, score, summary) tuples only, correcting
//      rubric-order violations. It never sees the payloads again, so it cannot become
//      a second, batched scoring round.
//   4. PERSIST — replace the day's rows for this mode (live vs replay never mix).
//
// THE CONTEXT CASCADE is mechanical, decided BEFORE any model call so each ticker
// still costs exactly one scoring request: Alpaca news → Massive news → open web.
// Escalation happens when a stage returns nothing, or only generic "stocks moving"
// list noise. Distinguishing "we looked everywhere and there is no catalyst" from
// "we could not look" matters more here than anywhere else in the app: the first is
// the strongest possible panic signal, the second is merely ignorance, and scoring
// them alike would systematically mis-rank the names this strategy exists to find.

pub mod payload;
pub mod prompt;
pub mod state;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

use futures_util::stream::{self, StreamExt};

use crate::config::secrets::Secrets;
use crate::llm::deepseek::{Deepseek, Effort, Request, MODEL_PRO};
use crate::local_db::panic_llm_repository::{self, PanicLlmScore};
use crate::local_db::{cache_repository, company_intel_repository, company_meta_repository};
use crate::local_db::{scoring_repository, universe_repository};
use crate::market_state::aggregators::Timeframe;
use crate::market_state::MarketState;
use payload::TickerPayload;
use state::{PanicRunState, Phase};

/// How many scoring calls may be in flight at once. Bounded so a 20-ticker watchlist
/// does not open 20 sockets or trip Deepseek's rate limit; 4 keeps a full run in the
/// 15–25 s range.
/// How many scoring calls may be in flight at once.
///
/// Sized against measured latency: a thinking-model call on this rubric takes 35–75 s,
/// so a 20-ticker watchlist at 4-wide would run ~6 minutes. At 8-wide it is ~3, which
/// is tolerable for a pre-open tool the user watches. Kept bounded rather than
/// unlimited so a large watchlist cannot open 30 sockets or trip the rate limit.
const CONCURRENCY: usize = 8;

/// Effort for the scoring calls.
///
/// MEASURED: `High` consumed the whole token budget on hidden reasoning and returned
/// empty content, while `Medium` answered reliably — and scored the same payload 93
/// against `Low`'s 94, so the extra thinking buys ~1 point on a 0–100 scale. The
/// rubric is explicit enough that the model does not need to re-derive it. Medium is
/// the balance between a stable score and a run that finishes.
const SCORE_EFFORT: Effort = Effort::Medium;

/// Fallback effort for the single retry after a truncated reply. Lower, so the retry
/// cannot fail the same way twice — measured at roughly half the reasoning budget.
const RETRY_EFFORT: Effort = Effort::Low;

/// Daily closes read per ticker. Driven by the Bollinger Z basis (`current_bbz` needs
/// 20), which is deeper than the payload's own daily window, so one read serves both.
const BBZ_DAILY_BARS: u32 = 30;

/// Process-wide exclusion. A second click while a run is in flight is ignored rather
/// than queued: the run replaces the whole day's rows, so two overlapping runs would
/// race to write the same (day, mode) key.
static RUN_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub type RunState = Arc<RwLock<PanicRunState>>;

/// Mirror a message into `local_logs` so it reaches Réglages → Logs.
///
/// `eprintln!` alone is not enough for this feature: the run is user-triggered and
/// can only fail in ways the user must be able to see (missing key, model refusing,
/// truncated reply). In a packaged build there is no terminal to read at all, so a
/// stderr-only failure is indistinguishable from the button doing nothing.
fn log_db(db: &Arc<Mutex<rusqlite::Connection>>, level: &str, msg: &str) {
    if let Ok(conn) = db.lock() {
        let _ = crate::local_db::insert_log(&conn, level, msg);
    }
}

// ─── News-noise classification (pure) ──────────────────────────────────────────

/// Headline fragments that carry no ticker-specific information. These are the
/// round-up articles that name dozens of symbols ("12 stocks moving in Tuesday's
/// pre-market session"), which Alpaca's feed emits constantly. Treating one as a
/// catalyst is the single most likely way to score a genuine no-news squeeze as
/// "explained", so they are filtered before the model ever sees them.
const NOISE_PATTERNS: &[&str] = &[
    "stocks moving",
    "stocks that are moving",
    "moving in ",
    "movers",
    "top gainers",
    "top losers",
    "biggest gainers",
    "biggest losers",
    "premarket movers",
    "pre-market movers",
    "after-hours movers",
    "market update",
    "stocks to watch",
    "watchlist",
    "unusual options activity",
    "why is",
    "here's what",
    "trending stocks",
    "gainers and losers",
    "market clubhouse",
    "recap",
];

/// True when a headline is a generic round-up rather than ticker-specific news.
pub fn is_noise_headline(headline: &str) -> bool {
    let h = headline.to_lowercase();
    NOISE_PATTERNS.iter().any(|p| h.contains(p))
}

/// Does this set of headlines contain at least one usable, ticker-specific item?
/// Empty and noise-only sets both answer false, which is what triggers escalation.
pub fn has_usable_news(headlines: &[String]) -> bool {
    headlines.iter().any(|h| !is_noise_headline(h))
}

// ─── Model answer parsing (pure) ───────────────────────────────────────────────

/// One ticker's parsed model answer.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ScoreAnswer {
    pub panic_score: f64,
    pub confidence_score: Option<f64>,
    pub catalyst_strength: Option<f64>,
    pub catalyst_type: Option<String>,
    pub catalyst_label: Option<String>,
    pub reversibility: Option<String>,
    pub normalization_prob: Option<f64>,
    pub forced_flow: Option<String>,
    pub euphoria: Option<String>,
    pub key_drivers: Vec<String>,
    pub one_liner: Option<String>,
    pub catalyst_direction: Option<String>,
    pub fundamental_reset: bool,
    pub veto_reason: Option<String>,
    pub timing_state: Option<String>,
}

/// Clamp a model-supplied 0..100 figure. Out-of-range values are pulled into range
/// rather than rejected: a 105 is a formatting slip, not a reason to lose the ticker.
fn clamp_score(v: f64) -> f64 {
    if !v.is_finite() {
        return 0.0;
    }
    v.clamp(0.0, 100.0)
}

fn opt_score(v: Option<f64>) -> Option<f64> {
    v.filter(|x| x.is_finite()).map(clamp_score)
}

fn opt_string(v: Option<&str>) -> Option<String> {
    v.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Parse a scoring answer. Tolerant of a fenced code block (the model occasionally
/// wraps JSON despite json mode) but strict about `panic_score`: without it there is
/// nothing to rank, so the ticker is reported as failed rather than defaulted to 0 —
/// a silent 0 would rank a real panic candidate last.
pub fn parse_score_answer(raw: &str) -> Result<ScoreAnswer, String> {
    let json = extract_json(raw).ok_or_else(|| {
        format!(
            "réponse non-json: {}",
            payload::truncate(raw, 160)
        )
    })?;
    let v: serde_json::Value =
        serde_json::from_str(&json).map_err(|e| format!("json invalide: {e}"))?;

    let panic = v
        .get("panic_score")
        .and_then(|x| x.as_f64())
        .filter(|x| x.is_finite())
        .ok_or_else(|| "panic_score absent ou non numérique".to_string())?;

    Ok(ScoreAnswer {
        panic_score: clamp_score(panic),
        confidence_score: opt_score(v.get("confidence_score").and_then(|x| x.as_f64())),
        catalyst_strength: opt_score(v.get("catalyst_strength").and_then(|x| x.as_f64())),
        catalyst_type: opt_string(v.get("catalyst_type").and_then(|x| x.as_str())),
        catalyst_label: opt_string(v.get("catalyst_label").and_then(|x| x.as_str())),
        reversibility: opt_string(v.get("reversibility").and_then(|x| x.as_str())),
        normalization_prob: opt_score(v.get("normalization_prob").and_then(|x| x.as_f64())),
        forced_flow: opt_string(v.get("forced_flow").and_then(|x| x.as_str())),
        euphoria: opt_string(v.get("euphoria").and_then(|x| x.as_str())),
        key_drivers: v
            .get("key_drivers")
            .and_then(|x| x.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|d| opt_string(d.as_str()))
                    .take(8)
                    .collect()
            })
            .unwrap_or_default(),
        one_liner: opt_string(v.get("one_liner").and_then(|x| x.as_str())),
        catalyst_direction: opt_string(v.get("catalyst_direction").and_then(|x| x.as_str())),
        fundamental_reset: v
            .get("fundamental_reset")
            .and_then(|x| x.as_bool())
            .unwrap_or(false),
        veto_reason: opt_string(v.get("veto_reason").and_then(|x| x.as_str())),
        timing_state: opt_string(v.get("timing_state").and_then(|x| x.as_str())),
    })
}

fn inferred_timing_state(p: &TickerPayload) -> String {
    let Some(direction) = p.quant_panic.direction else { return "unknown".into() };
    let recent = p.price.change_5m_pct;
    let broader = p.price.change_15m_pct;
    match (recent, broader) {
        (Some(r), _) if r.signum() as i8 == direction && r.abs() >= 0.25 => "extending".into(),
        (Some(r), _) if r.signum() as i8 == -direction && r.abs() >= 0.25 => "reversing".into(),
        (Some(_), Some(b)) if b.abs() >= 0.5 => "exhausting".into(),
        _ => "unknown".into(),
    }
}

fn catalyst_matches_move(direction: Option<i8>, catalyst_direction: Option<&str>) -> bool {
    matches!(
        (direction, catalyst_direction),
        (Some(1), Some("positive")) | (Some(-1), Some("negative"))
    )
}

fn material_catalyst_type(kind: Option<&str>) -> bool {
    matches!(
        kind,
        Some(
            "fda"
                | "earnings"
                | "ma"
                | "offering"
                | "clinical"
                | "legal"
                | "guidance"
                | "contract"
        )
    )
}

fn ranking_score(
    panic: f64,
    normalization: Option<f64>,
    confidence: Option<f64>,
    veto: bool,
    eligibility: &str,
) -> f64 {
    if veto {
        return 0.0;
    }
    let base = panic * 0.7 + normalization.unwrap_or(panic) * 0.3;
    let confidence_factor = 0.5 + 0.5 * confidence.unwrap_or(0.0).clamp(0.0, 100.0) / 100.0;
    let review_factor = if eligibility == "review" { 0.75 } else { 1.0 };
    (base * confidence_factor * review_factor).clamp(0.0, 100.0)
}

/// Pull the outermost JSON object out of a reply, tolerating ``` fences and stray
/// prose around it.
fn extract_json(raw: &str) -> Option<String> {
    let t = raw.trim();
    let start = t.find('{')?;
    let end = t.rfind('}')?;
    (end > start).then(|| t[start..=end].to_string())
}

/// One calibration adjustment.
#[derive(Debug, Clone, PartialEq)]
pub struct Adjustment {
    pub symbol: String,
    pub panic_score: f64,
    pub reason: String,
}

/// Parse the calibration reply. An unparseable answer yields an EMPTY adjustment list
/// rather than an error: calibration is a refinement, and losing it must never cost
/// the user a completed run's scores.
pub fn parse_calibration(raw: &str) -> Vec<Adjustment> {
    let Some(json) = extract_json(raw) else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&json) else {
        return Vec::new();
    };
    v.get("adjustments")
        .and_then(|a| a.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|a| {
                    let symbol = opt_string(a.get("symbol").and_then(|s| s.as_str()))?
                        .to_uppercase();
                    let score = a.get("panic_score").and_then(|s| s.as_f64())?;
                    // An adjustment without a stated reason is exactly the "spread the
                    // notes" behaviour the rubric forbids, so it is dropped.
                    let reason = opt_string(a.get("reason").and_then(|s| s.as_str()))?;
                    score.is_finite().then_some(Adjustment {
                        symbol,
                        panic_score: clamp_score(score),
                        reason,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Apply adjustments to the raw scores, enforcing the movement cap.
///
/// Returns the number applied. An adjustment beyond `MAX_ADJUSTMENT`, or naming a
/// ticker that was not in the run, is discarded: the calibration pass is allowed to
/// smooth the scale, never to re-score a ticker or invent one.
pub fn apply_adjustments(rows: &mut [PanicLlmScore], adjustments: &[Adjustment]) -> usize {
    let mut applied = 0;
    for adj in adjustments {
        let Some(row) = rows.iter_mut().find(|r| r.symbol == adj.symbol) else {
            continue;
        };
        let Some(raw) = row.panic_score_raw else {
            continue;
        };
        if (adj.panic_score - raw).abs() > prompt::MAX_ADJUSTMENT {
            eprintln!(
                "[tagdash] panic_llm: calibration refusée pour {} ({raw:.0} → {:.0}, > {} points)",
                adj.symbol, adj.panic_score, prompt::MAX_ADJUSTMENT,
            );
            continue;
        }
        row.panic_score = Some(adj.panic_score);
        row.adjustment_reason = Some(adj.reason.clone());
        applied += 1;
    }
    applied
}

// ─── Payload collection ────────────────────────────────────────────────────────

/// Everything the payload builder needs that comes from the DB. Gathered under one
/// short lock per ticker, never held across an `.await`.
struct DbFacts {
    payload: TickerPayload,
    company_name: Option<String>,
}

/// Build the DB-sourced part of a ticker's payload. Synchronous and lock-scoped.
/// `market_caps` is passed IN rather than resolved here: `sec_shares::market_caps_as_of`
/// builds a map over the WHOLE universe, so calling it per ticker would rebuild several
/// thousand entries once per name in the watchlist.
fn collect_db_facts(
    conn: &rusqlite::Connection,
    market: &Arc<RwLock<MarketState>>,
    market_caps: &HashMap<String, f64>,
    symbol: &str,
    day: &str,
    simulated: bool,
    feed: &str,
) -> DbFacts {
    let mut p = TickerPayload::default();
    let mut missing: Vec<String> = Vec::new();
    let mut lookahead: Vec<String> = Vec::new();

    // ── Identity
    let asset = universe_repository::get_by_symbol(conn, symbol).unwrap_or(None);
    let meta = company_meta_repository::get_by_symbol(conn, symbol).unwrap_or(None);
    p.identity.symbol = symbol.to_string();
    if let Some(a) = &asset {
        p.identity.name = a.name.clone();
        p.identity.exchange = a.exchange.clone();
        p.identity.security_type = a.security_type.clone();
        p.float_and_short.shortable = Some(a.shortable);
    } else {
        missing.push("identity.name".into());
    }
    if let Some(m) = &meta {
        p.identity.country = m.country.clone();
        p.identity.industry = m.industry.clone();
        p.identity.sector = m.sector.clone();
        p.identity.sic = m.sic.clone();
    } else {
        missing.push("identity.country/industry".into());
    }

    // ── Fundamentals + risk scores. These are CURRENT snapshots: in replay they
    //    describe today, not the replayed day, so they are declared as look-ahead
    //    rather than quietly presented as period-correct.
    let fund = cache_repository::get_fundamental(conn, symbol).unwrap_or(None);
    if let Some(f) = &fund {
        p.float_and_short.float_shares = f.float_shares;
        p.float_and_short.shares_outstanding = f.outstanding_shares;
        p.float_and_short.free_float_pct = f.free_float;
        p.price.prev_close = f.prev_close;
        p.volume.avg_daily_volume = f.avg_volume;
        p.volatility.atr20 = f.atr;
    } else {
        missing.push("float_and_short.float_shares".into());
    }
    let risk = cache_repository::get_risk_scores(conn, symbol).unwrap_or_default();
    p.risk_scores.pump_dump_score = risk.pump_dump_score;
    p.risk_scores.dilution_score = risk.dilution_score;
    p.risk_scores.dilution_capacity_score = risk.dilution_capacity_score;
    p.risk_scores.dilution_need_score = risk.dilution_need_score;
    p.risk_scores.short_interest_score = risk.short_interest_score;
    let dil = cache_repository::get_dilution_and_splits(conn, symbol).unwrap_or_default();
    p.risk_scores.dilution_pct_12m = dil.dilution_pct_12m;
    p.sec.last_split_date = dil.last_split_date;
    p.sec.last_split_label = dil.last_split_label;
    p.sec.splits_last_1y = dil.split_count_1y;
    if simulated {
        lookahead.push("float_and_short.*".into());
        lookahead.push("risk_scores.*".into());
        lookahead.push("sec.splits_last_1y".into());
    }

    if let Ok(Some(ch)) = cache_repository::multiday_changes(conn, symbol) {
        p.price.change_multiday_pct = ch.changes;
    }

    // ── Market cap (SEC-derived, as-of the day where available).
    p.float_and_short.market_cap = market_caps.get(symbol).copied();
    if p.float_and_short.market_cap.is_none() {
        missing.push("float_and_short.market_cap".into());
    }

    // ── The quantitative reading that selected this ticker.
    if let Ok(Some(s)) = scoring_repository::get_one(conn, day, symbol) {
        p.quant_panic.score = Some(s.score);
        p.quant_panic.sigma = Some(s.sigma);
        p.quant_panic.atr_expansion = Some(s.atr_exp);
        p.quant_panic.window_days = Some(s.window_days);
        p.quant_panic.direction = Some(s.direction);
        p.quant_panic.cap_bin = s.cap_bin;
        p.quant_panic.setup_kind = Some(s.setup_kind);
        p.quant_panic.move_pct = Some(s.move_pct);
        p.quant_panic.stability_score = Some(s.stability_score);
        p.quant_panic.volume_expansion = s.volume_expansion;
        p.quant_panic.directional_efficiency = s.directional_efficiency;
        p.quant_panic.consecutive_bars = s.consecutive_bars;
    } else {
        // A pinned ticker that no longer scores legitimately lands here.
        missing.push("quant_panic.*".into());
    }

    // ── Company intel: short interest, financials, dilution.
    if let Ok(Some(intel)) = company_intel_repository::get_intel(conn, symbol) {
        p.float_and_short.short_interest = intel.short_interest.short_interest;
        p.float_and_short.days_to_cover = intel.short_interest.days_to_cover;
        p.float_and_short.short_interest_settlement =
            intel.short_interest.settlement_date.clone();
        // Short interest as a share of float is the figure the rubric actually keys
        // on; the raw count means little without it.
        if let (Some(si), Some(fl)) = (
            intel.short_interest.short_interest,
            p.float_and_short.float_shares,
        ) {
            if fl > 0 {
                p.float_and_short.short_interest_pct_float =
                    Some(si as f64 / fl as f64 * 100.0);
            }
        }
        p.financials.net_income_last_q = intel.financials.net_income_last_q;
        p.financials.net_income_ttm = intel.financials.net_income_ttm;
        p.financials.negative_quarters_last4 = intel.financials.negative_quarters_last4;
        p.financials.operating_cash_flow_ttm = intel.financials.operating_cash_flow_ttm;
        p.financials.cash_and_equivalents = intel.financials.cash_and_equivalents;
        p.financials.period_end = intel.financials.period_end.clone();
        p.sec.has_recent_shelf = intel.dilution.has_recent_shelf;
        p.sec.latest_dilution_form = intel.dilution.latest_form.clone();
        p.sec.latest_dilution_date = intel.dilution.latest_date.clone();
        if intel.short_interest.is_empty() {
            missing.push("float_and_short.short_interest".into());
        }
        if intel.financials.is_empty() {
            missing.push("financials.*".into());
        }
    } else {
        missing.push("float_and_short.short_interest".into());
        missing.push("financials.*".into());
        missing.push("sec.dilution".into());
    }

    // ── Filings, DATE-BOUNDED so a replay never sees a filing from after the day.
    let filings =
        company_intel_repository::get_recent_filings_before(conn, symbol, day, payload::FILINGS_LIMIT)
            .unwrap_or_default();
    p.sec.recent_filings = filings
        .iter()
        .map(|f| payload::Filing {
            form_type: f.form_type.clone(),
            filing_date: f.filing_date.clone(),
            category: f.category.clone(),
            description: f.description.clone(),
            atm: f.detected_atm,
            resale: f.detected_resale,
            warrants: f.detected_warrants,
            offering_amount: f.offering_amount,
        })
        .collect();
    if p.sec.recent_filings.is_empty() {
        missing.push("sec.recent_filings".into());
    }

    // ── Daily bars, bounded at `day` so the replayed future is structurally absent.
    //    Read ONCE at the Bollinger depth (30) and sliced: the consecutive-day streak
    //    only needs the recent tail, and two queries for overlapping windows of the
    //    same series is pure waste.
    let mut daily = cache_repository::get_daily_bars_before(conn, symbol, day, BBZ_DAILY_BARS)
        .unwrap_or_default();
    daily.reverse(); // the getter is date-DESC
    let closes_asc: Vec<f64> = daily.iter().filter_map(|b| b.close).collect();
    let tail_from = closes_asc
        .len()
        .saturating_sub(payload::DAILY_BARS as usize);
    p.volatility.consecutive_days = payload::consecutive_direction(&closes_asc[tail_from..]);
    p.quant_panic.movement_start = match p.quant_panic.setup_kind.as_deref() {
        Some("premarket") => Some(day.to_string()),
        Some("multiday") => p.quant_panic.window_days.and_then(|w| {
            let w = w as usize;
            (w > 0 && daily.len() >= w)
                .then(|| daily[daily.len() - w].date.clone())
        }),
        _ => None,
    };

    // ── Live/intraday state from RAM. In a replay the tape feeds the same store, so
    //    this path is identical in both modes — only availability differs.
    let (live, m1, live_price) = {
        let ms = market.read().unwrap();
        let live = ms.tickers.get(symbol).cloned();
        let m1 = ms.get_bars(symbol, Timeframe::M1);
        let price = ms.last_price(symbol);
        (live, m1, price)
    };

    if let Some(t) = &live {
        p.price.last = t.last_price.or(live_price);
        p.price.hod = t.high_day;
        p.price.lod = t.low_day;
        p.price.vwap = t.vwap;
        p.price.change_from_prev_close_pct = t.change_day_pct;
        p.volume.volume_since_open = Some(t.volume_day as i64);
        p.microstructure.bid = t.bid;
        p.microstructure.ask = t.ask;
        p.microstructure.spread = t.spread;
        if let (Some(sp), Some(px)) = (t.spread, t.last_price) {
            if px > 0.0 {
                p.microstructure.spread_pct = Some(sp / px * 100.0);
            }
        }
        if t.previous_close.is_some() {
            p.price.prev_close = t.previous_close;
        }
        if let (Some(px), Some(vw)) = (p.price.last, t.vwap) {
            p.price.distance_to_vwap_pct = payload::pct_change(vw, px);
        }
        if let (Some(px), Some(vol)) = (p.price.last, Some(t.volume_day)) {
            p.volume.dollar_volume_today = Some(px * vol as f64);
        }
    } else {
        p.price.last = live_price.or_else(|| closes_asc.last().copied());
        missing.push("microstructure.*".into());
        missing.push("volume.volume_since_open".into());
    }
    if p.microstructure.bid.is_none() {
        missing.push("microstructure.bid/ask".into());
    }

    // ── Bollinger Z on the daily basis (needs ≥20 closes — hence BBZ_DAILY_BARS).
    if let Some(px) = p.price.last {
        p.price.bollinger_z = crate::scoring::current_bbz(&closes_asc, px);
    }

    // ── Intraday derived metrics over the minute ring, today only.
    let today_m1: Vec<crate::market_state::aggregators::Bar> = m1
        .iter()
        .filter(|b| crate::time::et_date(b.time) == day)
        .cloned()
        .collect();
    if today_m1.is_empty() {
        missing.push("volume.volume_1m/5m".into());
        missing.push("volatility.directional_efficiency".into());
    } else {
        if let Some(first) = today_m1.first() {
            p.price.open_today = Some(first.open);
            p.price.change_from_open_pct =
                p.price.last.and_then(|px| payload::pct_change(first.open, px));
        }
        p.price.change_1m_pct = payload::change_over_minutes(&today_m1, 1);
        p.price.change_5m_pct = payload::change_over_minutes(&today_m1, 5);
        p.price.change_15m_pct = payload::change_over_minutes(&today_m1, 15);
        p.volume.volume_1m = payload::volume_over_minutes(&today_m1, 1);
        p.volume.volume_5m = payload::volume_over_minutes(&today_m1, 5);
        p.volume.volume_acceleration = payload::volume_acceleration(&today_m1, 5);
        p.volume.trade_frequency_per_min = payload::trade_frequency_per_min(&today_m1);
        if p.volume.trade_frequency_per_min.is_none() {
            missing.push("volume.trade_frequency_per_min".into());
        }

        // The momentum window: the tail of today's session.
        let tail_from = today_m1.len().saturating_sub(payload::INTRADAY_LOOKBACK_MIN);
        let tail = &today_m1[tail_from..];
        p.volatility.directional_efficiency = payload::directional_efficiency(tail);
        p.volatility.max_pullback_pct = payload::max_pullback_pct(tail);
        p.volatility.price_acceleration = payload::price_acceleration(&today_m1, 5);
        let tail_closes: Vec<f64> = tail.iter().map(|b| b.close).collect();
        p.volatility.consecutive_bars = payload::consecutive_direction(&tail_closes);

        // Premarket slice (04:00–09:30 ET), the pre-open liquidity read.
        let pm: Vec<_> = today_m1
            .iter()
            .filter(|b| (240..570).contains(&crate::time::et_minutes(b.time)))
            .collect();
        if !pm.is_empty() {
            let vol: i64 = pm.iter().map(|b| b.volume as i64).sum();
            p.volume.premarket_volume = Some(vol);
            p.volume.premarket_dollar_volume = Some(
                pm.iter()
                    .map(|b| b.vwap.unwrap_or(b.close) * b.volume as f64)
                    .sum(),
            );
        } else {
            missing.push("volume.premarket_volume".into());
        }

        // Time-of-day RVOL: today's cumulative volume at this clock position versus
        // the same position on the prior cached sessions. Pre-open a whole-day RVOL is
        // ~0 for every ticker and discriminates nothing, which is why this exists.
        let now_min = crate::time::et_minutes(crate::time::now());
        let today_to_now: i64 = today_m1
            .iter()
            .filter(|b| crate::time::et_minutes(b.time) <= now_min)
            .map(|b| b.volume as i64)
            .sum();
        let prior: Vec<i64> = m1
            .iter()
            .filter(|b| {
                let d = crate::time::et_date(b.time);
                d != day && crate::time::et_minutes(b.time) <= now_min
            })
            .fold(HashMap::<String, i64>::new(), |mut acc, b| {
                *acc.entry(crate::time::et_date(b.time)).or_insert(0) += b.volume as i64;
                acc
            })
            .into_values()
            .collect();
        p.volume.rvol_time_of_day = payload::rvol_time_of_day(today_to_now, &prior);
        if p.volume.rvol_time_of_day.is_none() {
            missing.push("volume.rvol_time_of_day".into());
        }
    }

    // Whole-session RVOL from the universe average.
    if let (Some(v), Some(avg)) = (p.volume.volume_since_open, p.volume.avg_daily_volume) {
        if avg > 0 {
            p.volume.rvol_intraday = Some(v as f64 / avg as f64);
        }
    }
    // ATR as a share of price, and today's range against it.
    if let (Some(atr), Some(px)) = (p.volatility.atr20, p.price.last) {
        if px > 0.0 {
            p.volatility.atr_pct_of_price = Some(atr / px * 100.0);
        }
        if atr > 0.0 {
            if let (Some(h), Some(l)) = (p.price.hod, p.price.lod) {
                p.volatility.range_over_atr = Some((h - l) / atr);
            }
        }
    }
    if p.volatility.atr20.is_none() {
        missing.push("volatility.atr20".into());
    }

    // Short volume is not collected anywhere — declared so the model reads it as
    // unavailable rather than as zero.
    missing.push("float_and_short.short_volume".into());

    // ── Data quality + run context.
    p.data_quality.feed = feed.to_string();
    p.data_quality.iex_only = if feed == "iex" {
        vec![
            "volume.*".into(),
            "price.vwap".into(),
            "price.hod".into(),
            "price.lod".into(),
            "microstructure.*".into(),
            "volume.rvol_intraday".into(),
        ]
    } else {
        Vec::new()
    };
    missing.sort();
    missing.dedup();
    p.data_quality.missing = missing;
    p.data_quality.lookahead = lookahead;

    let now = crate::time::now();
    p.context.mode = if simulated { "replay" } else { "live" }.to_string();
    p.context.as_of = now.format("%Y-%m-%dT%H:%M:%SZ").to_string();
    p.context.trading_day = day.to_string();
    p.context.session = format!("{:?}", crate::time::session_at(now)).to_lowercase();
    p.context.snapshot_is_current = simulated;

    DbFacts {
        payload: p,
        company_name: asset.and_then(|a| a.name),
    }
}

/// Run the context cascade for one ticker and fill `news` + `context_sources`.
fn news_lookback_days(movement_start: Option<&str>, trading_day: &str) -> i64 {
    movement_start
        .and_then(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
        .and_then(|start| {
            chrono::NaiveDate::parse_from_str(trading_day, "%Y-%m-%d")
                .ok()
                .map(|end| (end - start).num_days() + 2)
        })
        .unwrap_or(payload::NEWS_DAYS)
        .clamp(payload::NEWS_DAYS, payload::NEWS_DAYS_MAX)
}

async fn fill_context(
    p: &mut TickerPayload,
    secrets: &Arc<RwLock<Secrets>>,
    symbol: &str,
    company_name: Option<&str>,
) {
    let (alpaca, massive_key) = {
        let s = secrets.read().unwrap();
        (
            s.alpaca_key.clone().zip(s.alpaca_secret.clone()),
            s.massive_api_key.clone(),
        )
    };

    // Align the catalyst search to the winning movement, not to an arbitrary five
    // calendar-day window. Five trading sessions can span two weekends; the cap keeps
    // payload size/network cost bounded.
    let news_days = news_lookback_days(
        p.quant_panic.movement_start.as_deref(),
        &p.context.trading_day,
    );
    // The most extreme moves deserve an independent context check even when a feed
    // returned one non-generic headline; a mere ticker mention must not become the
    // sole explanation for a 100% repricing.
    let force_web_crosscheck = p.quant_panic.move_pct.unwrap_or(0.0).abs() >= 40.0;

    // ── Stage 1: Alpaca news. Already replay-bounded (`end` capped at the simulated
    //    instant), so no extra guard is needed here.
    let mut articles = Vec::new();
    if let Some((k, sec)) = &alpaca {
        match crate::alpaca::news::fetch_recent_news(
            k,
            sec,
            symbol,
            news_days,
            payload::NEWS_LIMIT,
        )
        .await
        {
            Ok(a) => articles = a,
            Err(e) => eprintln!("[tagdash] panic_llm: news {symbol}: {e}"),
        }
    }
    p.context_sources.alpaca_news_count = articles.len();
    let now = crate::time::now();
    // The escalation decision goes through `has_usable_news` so the rule the tests
    // pin is the rule that actually runs. Noise headlines are then dropped rather
    // than passed through: leaving one in hands the model a "catalyst" that names
    // fifty tickers.
    let headlines: Vec<String> = articles.iter().map(|a| a.headline.clone()).collect();
    if has_usable_news(&headlines) {
        for a in articles.iter().filter(|a| !is_noise_headline(&a.headline)) {
            let body = {
                let stripped = payload::strip_html(&a.content);
                let text = if stripped.trim().is_empty() {
                    a.summary.clone()
                } else {
                    stripped
                };
                payload::truncate(&text, payload::NEWS_BODY_CHARS)
            };
            p.news.push(payload::NewsItem {
                published_at: a.created_at.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
                hours_ago: Some((now - a.created_at).num_minutes() as f64 / 60.0),
                headline: a.headline.clone(),
                source: Some(a.source.clone()).filter(|s| !s.is_empty()),
                body_excerpt: Some(body).filter(|b| !b.is_empty()),
            });
        }
        p.context_sources.stage = "alpaca_news".into();
        if !force_web_crosscheck {
            return;
        }
    }

    // ── Stage 2: Massive news.
    //
    // NOTE: this endpoint's URL and field names are UNVERIFIED in the codebase (see
    // `massive::news`), so a failure here is expected rather than exceptional and the
    // cascade simply continues.
    if let Some(mk) = massive_key.filter(|k| !k.trim().is_empty()) {
        match crate::massive::news::fetch_latest_news(&mk, symbol).await {
            Ok(items) => {
                p.context_sources.massive_news_count = items.len();
                let titles: Vec<String> = items.iter().map(|i| i.title.clone()).collect();
                if has_usable_news(&titles) {
                    for it in items.into_iter().filter(|i| !is_noise_headline(&i.title)) {
                        p.news.push(payload::NewsItem {
                            published_at: it.published_at.clone().unwrap_or_default(),
                            hours_ago: None,
                            headline: it.title,
                            source: Some("Massive".into()),
                            body_excerpt: None,
                        });
                    }
                    p.context_sources.stage = "massive_news".into();
                    if !force_web_crosscheck {
                        return;
                    }
                }
            }
            Err(e) => eprintln!("[tagdash] panic_llm: massive news {symbol}: {e}"),
        }
    }

    // ── Stage 3: open web. Refuses itself during a replay.
    match crate::websearch::search(secrets, symbol, company_name).await {
        Ok(res) => {
            p.context_sources.stage = "web".into();
            p.context_sources.web_used = true;
            p.context_sources.web_provider = Some(res.provider);
            p.context_sources.web_answer = res.answer;
            p.context_sources.web_hits = res
                .hits
                .into_iter()
                .map(|h| payload::WebHit {
                    title: h.title,
                    url: h.url,
                    snippet: h.snippet,
                })
                .collect();
        }
        Err(reason) => {
            // "We could not look" is recorded distinctly from "we looked and found
            // nothing" — the rubric scores the two differently on purpose.
            p.context_sources.stage = "aucune".into();
            p.context_sources.web_skipped_reason = Some(reason.reason());
        }
    }
}

// ─── Run ───────────────────────────────────────────────────────────────────────

/// Score every symbol in `symbols` and replace the day's stored analysis.
///
/// `symbols` comes from the FRONTEND — the visible watchlist after the user's
/// dismissals. The backend deliberately does not re-derive it: the user removes
/// technically poor candidates in the UI, and re-deriving would re-analyse names they
/// just discarded.
pub async fn run(
    symbols: Vec<String>,
    db: Arc<Mutex<rusqlite::Connection>>,
    secrets: Arc<RwLock<Secrets>>,
    market: Arc<RwLock<MarketState>>,
    run_state: RunState,
) {
    let Ok(_guard) = RUN_LOCK.try_lock() else {
        // A second click while a run is in flight. The in-flight run owns the day's
        // rows; queueing a duplicate would race it for the same key.
        return;
    };

    let now = crate::time::now();
    let day = crate::time::et_date(now);
    let simulated = crate::replay::clock::is_active();

    let symbols: Vec<String> = {
        let mut seen = std::collections::HashSet::new();
        symbols
            .into_iter()
            .map(|s| s.trim().to_uppercase())
            .filter(|s| !s.is_empty() && seen.insert(s.clone()))
            .collect()
    };

    run_state
        .write()
        .unwrap()
        .begin(&day, simulated, symbols.len());

    let Some(ds_key) = ({
        let s = secrets.read().unwrap();
        s.deepseek_api_key.clone().filter(|k| !k.trim().is_empty())
    }) else {
        let why = "clé Deepseek absente — Réglages → API Keys";
        log_db(&db, "error", &format!("panic score: {why}"));
        run_state.write().unwrap().abort(why);
        return;
    };
    if symbols.is_empty() {
        run_state
            .write()
            .unwrap()
            .abort("aucun ticker visible à analyser");
        return;
    }

    // The market-data plan decides how the model should read volume/spread figures.
    let feed = {
        let conn = db.lock().unwrap();
        cache_repository::get_app_meta(&conn, crate::alpaca::plan::PLAN_META_KEY)
            .as_deref()
            .and_then(crate::alpaca::plan::Plan::from_meta)
            .map(|p| p.as_str().to_string())
            .unwrap_or_else(|| "inconnu".to_string())
    };

    // ── 1. Collect payloads. The market-cap map spans the whole universe, so it is
    //    built ONCE for the run rather than per ticker.
    let market_caps = {
        let conn = db.lock().unwrap();
        crate::sec_shares::market_caps_as_of(&conn, &day)
    };
    let mut prepared: Vec<(String, TickerPayload)> = Vec::with_capacity(symbols.len());
    for symbol in &symbols {
        let facts = {
            let conn = db.lock().unwrap();
            collect_db_facts(&conn, &market, &market_caps, symbol, &day, simulated, &feed)
        };
        let mut p = facts.payload;
        fill_context(&mut p, &secrets, symbol, facts.company_name.as_deref()).await;
        prepared.push((symbol.clone(), p));
    }

    // ── 2. Score, one isolated call per ticker.
    run_state.write().unwrap().set_phase(Phase::Scoring);
    let ds = Arc::new(Deepseek::new(ds_key));
    let results: Vec<(String, Result<ScoreAnswer, String>, TickerPayload)> =
        stream::iter(prepared.into_iter().map(|(symbol, p)| {
            let ds = ds.clone();
            let run_state = run_state.clone();
            async move {
                let body = p.to_json();
                let user = prompt::score_user_message(&symbol, &body);
                let call = |effort: Effort| Request {
                    model: MODEL_PRO,
                    system: prompt::SYSTEM_RUBRIC,
                    user: &user,
                    effort: Some(effort),
                    json: true,
                    max_tokens: prompt::SCORE_MAX_TOKENS,
                    temperature: 0.0,
                };
                let mut out = ds.chat(call(SCORE_EFFORT)).await;
                // ONE retry at lower effort when the budget was exhausted by hidden
                // reasoning. Truncation is the only failure that loses a ticker
                // outright, and it depends on how long the model chose to think — so
                // it is worth one cheaper attempt rather than dropping the name.
                if matches!(&out, Err(e) if e.contains("tronquée")) {
                    eprintln!("[tagdash] panic_llm {symbol}: tronqué, retry en effort réduit");
                    out = ds.chat(call(RETRY_EFFORT)).await;
                }
                let parsed = match out {
                    Ok(ans) => {
                        // Cache hits are the runtime proof that the rubric prefix is
                        // byte-identical across calls — the basis of comparability.
                        eprintln!(
                            "[tagdash] panic_llm {symbol}: prompt {} (cache hit {}), sortie {}",
                            ans.usage.prompt, ans.usage.cache_hit, ans.usage.completion,
                        );
                        parse_score_answer(&ans.content)
                    }
                    Err(e) => Err(e),
                };
                {
                    let mut st = run_state.write().unwrap();
                    st.tick();
                    if parsed.is_err() {
                        st.fail(&symbol);
                    }
                }
                (symbol, parsed, p)
            }
        }))
        .buffer_unordered(CONCURRENCY)
        .collect()
        .await;

    let mut rows: Vec<PanicLlmScore> = Vec::new();
    // The first concrete failure, carried into the abort message. A bare "no ticker
    // could be scored" is what made the first version of this feature undiagnosable:
    // the real reason (a truncated reply) existed only in the dev terminal.
    let mut first_error: Option<String> = None;
    for (symbol, parsed, p) in results {
        // A ticker that failed to score is simply absent from the day's rows (already
        // counted in `state.failed`), so the rest of the analysis still lands.
        let a = match parsed {
            Ok(a) => a,
            Err(e) => {
                eprintln!("[tagdash] panic_llm {symbol}: {e}");
                // Also to the DB, so Réglages → Logs shows it. A user-triggered
                // feature that fails must leave a trace the user can actually reach.
                log_db(&db, "error", &format!("panic score {symbol}: {e}"));
                if first_error.is_none() {
                    first_error = Some(format!("{symbol}: {e}"));
                }
                continue;
            }
        };
        let quality = serde_json::to_string(&p.data_quality).ok();
        let trade_direction = match p.quant_panic.direction {
            Some(d) if d > 0 => Some("short".to_string()),
            Some(d) if d < 0 => Some("long".to_string()),
            _ => None,
        };
        let catalyst_direction = a
            .catalyst_direction
            .as_deref()
            .filter(|v| matches!(*v, "positive" | "negative" | "neutral" | "unknown"))
            .unwrap_or("unknown")
            .to_string();
        let strength = a.catalyst_strength.unwrap_or(0.0);
        let fundamental_veto = (a.fundamental_reset
            || (strength >= 80.0 && material_catalyst_type(a.catalyst_type.as_deref())))
            && strength >= 60.0
            && catalyst_matches_move(
                p.quant_panic.direction,
                Some(catalyst_direction.as_str()),
            );
        let low_confidence = a.confidence_score.unwrap_or(0.0) < 50.0;
        let direction_uncertain = strength >= 60.0 && catalyst_direction == "unknown";
        let unavailable_short = trade_direction.as_deref() == Some("short")
            && p.float_and_short.shortable == Some(false);
        let eligibility = if fundamental_veto {
            "veto"
        } else if low_confidence || unavailable_short || direction_uncertain {
            "review"
        } else {
            "eligible"
        };
        let deterministic_timing = inferred_timing_state(&p);
        let timing_state = if deterministic_timing != "unknown" {
            deterministic_timing
        } else {
            a.timing_state
                .as_deref()
                .filter(|v| {
                    matches!(*v, "extending" | "exhausting" | "reversing" | "unknown")
                })
                .unwrap_or("unknown")
                .to_string()
        };
        let adjusted_rank = ranking_score(
            a.panic_score,
            a.normalization_prob,
            a.confidence_score,
            fundamental_veto,
            eligibility,
        );
        let veto_reason = fundamental_veto.then_some(
            a.veto_reason
                .clone()
                .unwrap_or_else(|| "Le catalyseur fondamental justifie le mouvement.".into()),
        );
        rows.push(PanicLlmScore {
            symbol,
            day: day.clone(),
            simulated,
            panic_score: Some(a.panic_score),
            panic_score_raw: Some(a.panic_score),
            confidence_score: a.confidence_score,
            catalyst_strength: a.catalyst_strength,
            catalyst_label: a.catalyst_label,
            catalyst_type: a.catalyst_type,
            reversibility: a.reversibility,
            one_liner: a.one_liner,
            key_drivers: a.key_drivers,
            forced_flow: a.forced_flow,
            euphoria: a.euphoria,
            normalization_prob: a.normalization_prob,
            data_quality: quality,
            web_used: p.context_sources.web_used,
            web_provider: p.context_sources.web_provider.clone(),
            adjustment_reason: None,
            rubric_version: Some(prompt::RUBRIC_VERSION.to_string()),
            model: Some(MODEL_PRO.to_string()),
            ranking_score: Some(adjusted_rank),
            trade_direction,
            timing_state: Some(timing_state),
            fundamental_veto,
            veto_reason,
            eligibility: Some(eligibility.into()),
            catalyst_direction: Some(catalyst_direction),
            created_at: None,
        });
    }

    if rows.is_empty() {
        let why = first_error
            .map(|e| format!("aucun ticker noté — {e}"))
            .unwrap_or_else(|| "aucun ticker n'a pu être noté".to_string());
        log_db(&db, "error", &format!("panic score: {why}"));
        run_state.write().unwrap().abort(why);
        return;
    }

    // ── 3. Calibration. Only worth a call with at least two scores to compare.
    if rows.len() > 1 {
        run_state.write().unwrap().set_phase(Phase::Calibrating);
        let entries: Vec<prompt::CalibrationEntry> = rows
            .iter()
            .map(|r| prompt::CalibrationEntry {
                symbol: r.symbol.clone(),
                panic_score: r.panic_score.unwrap_or_default(),
                catalyst_strength: r.catalyst_strength,
                catalyst_label: r.catalyst_label.clone(),
                confidence_score: r.confidence_score,
                reversibility: r.reversibility.clone(),
                key_drivers: r.key_drivers.clone(),
                one_liner: r.one_liner.clone(),
            })
            .collect();
        let user = prompt::calibration_user_message(&entries);
        match ds
            .chat(Request {
                model: MODEL_PRO,
                system: prompt::SYSTEM_CALIBRATION,
                user: &user,
                effort: Some(Effort::Medium),
                json: true,
                max_tokens: prompt::CALIBRATION_MAX_TOKENS,
                temperature: 0.0,
            })
            .await
        {
            Ok(ans) => {
                let adjustments = parse_calibration(&ans.content);
                let applied = apply_adjustments(&mut rows, &adjustments);
                run_state.write().unwrap().calibrated = applied > 0;
                eprintln!(
                    "[tagdash] panic_llm: calibration — {} proposée(s), {applied} appliquée(s)",
                    adjustments.len(),
                );
            }
            // A failed calibration must not cost the run: the raw scores are already
            // comparable by construction, calibration only smooths them.
            Err(e) => eprintln!("[tagdash] panic_llm: calibration échouée: {e}"),
        }
    }

    // Calibration may have changed the raw reversibility score. Recompute the
    // confidence-aware rank from the final value before persistence.
    for row in &mut rows {
        let eligibility = row.eligibility.as_deref().unwrap_or("review");
        row.ranking_score = row.panic_score.map(|panic| {
            ranking_score(
                panic,
                row.normalization_prob,
                row.confidence_score,
                row.fundamental_veto,
                eligibility,
            )
        });
    }

    // ── 4. Persist.
    {
        let conn = db.lock().unwrap();
        if let Err(e) = panic_llm_repository::replace_for_day(&conn, &day, simulated, &rows) {
            run_state
                .write()
                .unwrap()
                .abort(format!("écriture impossible: {e}"));
            return;
        }
        let _ = panic_llm_repository::prune(&conn);
    }
    let failed = run_state.read().unwrap().failed.len();
    let summary = format!(
        "panic score {day}: {} ticker(s) noté(s){} (feed={feed}{})",
        rows.len(),
        if failed > 0 {
            format!(", {failed} échec(s)")
        } else {
            String::new()
        },
        if simulated { ", replay" } else { "" },
    );
    eprintln!("[tagdash] {summary}");
    // Logged on SUCCESS too: it is the record that tells the user which run produced
    // the ranking they are looking at, and whether any ticker was left out of it.
    log_db(&db, if failed > 0 { "warn" } else { "info" }, &summary);
    run_state.write().unwrap().finish();
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Cascade escalation ─────────────────────────────────────────────────────

    #[test]
    fn generic_roundup_headlines_are_noise() {
        for h in [
            "12 Stocks Moving In Tuesday's Pre-Market Session",
            "Top Gainers and Losers Today",
            "Premarket Movers: What To Watch",
            "MARKET UPDATE: indices mixed",
            "Stocks To Watch Thursday",
            "Why Is MTEN Stock Trading Higher Today?",
        ] {
            assert!(is_noise_headline(h), "should be noise: {h}");
        }
    }

    #[test]
    fn ticker_specific_headlines_are_not_noise() {
        for h in [
            "M-tron Industries Receives FDA Clearance For Its Lead Device",
            "Acme Corp Announces $40M Registered Direct Offering",
            "Beta Pharma Reports Positive Phase 3 Results",
            "Gamma Inc To Be Acquired By Delta At $12.00 Per Share In Cash",
        ] {
            assert!(!is_noise_headline(h), "should be usable: {h}");
        }
    }

    /// The escalation rule: an empty set and a noise-only set both mean "keep
    /// looking". Without the second case a round-up article would masquerade as a
    /// catalyst and a genuine no-news squeeze would score as explained.
    #[test]
    fn escalation_treats_noise_only_like_no_news_at_all() {
        assert!(!has_usable_news(&[]));
        assert!(!has_usable_news(&[
            "10 Stocks Moving In Friday's Session".to_string(),
            "Top Gainers Today".to_string(),
        ]));
        assert!(has_usable_news(&[
            "Top Gainers Today".to_string(),
            "Acme Announces FDA Approval".to_string(),
        ]));
    }

    #[test]
    fn news_window_covers_the_start_of_a_multiday_move() {
        assert_eq!(news_lookback_days(Some("2026-08-07"), "2026-08-14"), 9);
        assert_eq!(news_lookback_days(Some("2026-08-14"), "2026-08-14"), 5);
        assert_eq!(news_lookback_days(Some("bad"), "2026-08-14"), 5);
        assert_eq!(news_lookback_days(Some("2026-07-01"), "2026-08-14"), 14);
    }

    // ── Answer parsing ─────────────────────────────────────────────────────────

    fn valid_answer() -> &'static str {
        r#"{"panic_score":91,"confidence_score":72,"catalyst_strength":5,
            "catalyst_type":"aucun","catalyst_label":"aucun catalyseur identifié",
            "catalyst_direction":"unknown","fundamental_reset":false,
            "veto_reason":"","timing_state":"extending",
            "reversibility":"forte","normalization_prob":78,
            "forced_flow":"SI 25% et HTB.","euphoria":"Pullbacks inexistants.",
            "key_drivers":["float 4M","SI 25%"],"one_liner":"Phrase."}"#
    }

    #[test]
    fn a_complete_answer_parses_every_field() {
        let a = parse_score_answer(valid_answer()).expect("parses");
        assert_eq!(a.panic_score, 91.0);
        assert_eq!(a.confidence_score, Some(72.0));
        assert_eq!(a.catalyst_strength, Some(5.0));
        assert_eq!(a.reversibility.as_deref(), Some("forte"));
        assert_eq!(a.key_drivers, vec!["float 4M", "SI 25%"]);
        assert_eq!(a.one_liner.as_deref(), Some("Phrase."));
        assert_eq!(a.catalyst_direction.as_deref(), Some("unknown"));
        assert!(!a.fundamental_reset);
        assert_eq!(a.timing_state.as_deref(), Some("extending"));
    }

    #[test]
    fn fundamental_catalyst_must_match_the_move_direction() {
        assert!(catalyst_matches_move(Some(1), Some("positive")));
        assert!(catalyst_matches_move(Some(-1), Some("negative")));
        assert!(!catalyst_matches_move(Some(-1), Some("positive")));
        assert!(!catalyst_matches_move(Some(1), Some("negative")));
        assert!(!catalyst_matches_move(None, Some("positive")));
        assert!(material_catalyst_type(Some("offering")));
        assert!(!material_catalyst_type(Some("promo")));
    }

    #[test]
    fn confidence_and_veto_reduce_the_contextual_rank() {
        let high_conf = ranking_score(90.0, Some(80.0), Some(90.0), false, "eligible");
        let low_conf = ranking_score(90.0, Some(80.0), Some(20.0), false, "review");
        assert!(high_conf > low_conf);
        assert_eq!(ranking_score(99.0, Some(99.0), Some(99.0), true, "veto"), 0.0);
    }

    #[test]
    fn timing_phase_is_descriptive_and_direction_aware() {
        let mut p = TickerPayload::default();
        p.quant_panic.direction = Some(1);
        p.price.change_5m_pct = Some(1.2);
        assert_eq!(inferred_timing_state(&p), "extending");
        p.price.change_5m_pct = Some(-0.8);
        assert_eq!(inferred_timing_state(&p), "reversing");
        p.price.change_5m_pct = Some(0.05);
        p.price.change_15m_pct = Some(3.0);
        assert_eq!(inferred_timing_state(&p), "exhausting");
    }

    /// A missing panic_score is a HARD failure. Defaulting it to 0 would rank a real
    /// panic candidate last — worse than reporting the ticker as failed.
    #[test]
    fn a_missing_panic_score_fails_rather_than_defaulting_to_zero() {
        let err = parse_score_answer(r#"{"confidence_score":80,"one_liner":"x"}"#).unwrap_err();
        assert!(err.contains("panic_score"), "got: {err}");
        assert!(parse_score_answer(r#"{"panic_score":"beaucoup"}"#).is_err());
        assert!(parse_score_answer(r#"{"panic_score":null}"#).is_err());
    }

    /// Everything EXCEPT panic_score degrades to None so one sloppy field cannot cost
    /// the whole ticker.
    #[test]
    fn optional_fields_degrade_without_losing_the_ticker() {
        let a = parse_score_answer(r#"{"panic_score":55}"#).expect("parses");
        assert_eq!(a.panic_score, 55.0);
        assert_eq!(a.confidence_score, None);
        assert!(a.key_drivers.is_empty());
        assert_eq!(a.one_liner, None);
        // Blank strings are treated as absent, not as empty content to render.
        let b = parse_score_answer(r#"{"panic_score":55,"one_liner":"   "}"#).unwrap();
        assert_eq!(b.one_liner, None);
    }

    #[test]
    fn out_of_range_scores_are_clamped() {
        assert_eq!(parse_score_answer(r#"{"panic_score":140}"#).unwrap().panic_score, 100.0);
        assert_eq!(parse_score_answer(r#"{"panic_score":-20}"#).unwrap().panic_score, 0.0);
        let a = parse_score_answer(r#"{"panic_score":50,"confidence_score":250}"#).unwrap();
        assert_eq!(a.confidence_score, Some(100.0));
    }

    #[test]
    fn a_fenced_or_prefixed_reply_still_parses() {
        let fenced = format!("```json\n{}\n```", valid_answer());
        assert_eq!(parse_score_answer(&fenced).unwrap().panic_score, 91.0);
        let chatty = format!("Voici mon analyse:\n{}\nVoilà.", valid_answer());
        assert_eq!(parse_score_answer(&chatty).unwrap().panic_score, 91.0);
    }

    /// A truncated reply must fail cleanly — never panic, never half-parse.
    #[test]
    fn a_truncated_reply_fails_cleanly() {
        let cut = r#"{"panic_score":91,"one_liner":"Phrase incomp"#;
        assert!(parse_score_answer(cut).is_err());
        assert!(parse_score_answer("").is_err());
        assert!(parse_score_answer("pas de json ici").is_err());
        assert!(parse_score_answer("}{").is_err());
    }

    // ── Calibration ────────────────────────────────────────────────────────────

    fn rows_for_calibration() -> Vec<PanicLlmScore> {
        vec![
            PanicLlmScore {
                symbol: "AAAA".into(),
                panic_score: Some(55.0),
                panic_score_raw: Some(55.0),
                ..Default::default()
            },
            PanicLlmScore {
                symbol: "BBBB".into(),
                panic_score: Some(74.0),
                panic_score_raw: Some(74.0),
                ..Default::default()
            },
        ]
    }

    #[test]
    fn a_justified_adjustment_within_the_cap_is_applied() {
        let mut rows = rows_for_calibration();
        let adj = parse_calibration(
            r#"{"adjustments":[{"symbol":"AAAA","panic_score":68,
                "reason":"Profil identique à BBBB, écart injustifié."}]}"#,
        );
        assert_eq!(adj.len(), 1);
        assert_eq!(apply_adjustments(&mut rows, &adj), 1);
        assert_eq!(rows[0].panic_score, Some(68.0));
        assert!(rows[0].adjustment_reason.is_some());
        // The raw score is preserved so the drift stays auditable.
        assert_eq!(rows[0].panic_score_raw, Some(55.0));
        assert_eq!(rows[1].panic_score, Some(74.0), "untouched ticker unchanged");
    }

    /// The cap is what stops the calibration pass from silently re-scoring. A 30-point
    /// move is a new opinion about the ticker, not a scale correction.
    #[test]
    fn an_adjustment_beyond_the_cap_is_refused() {
        let mut rows = rows_for_calibration();
        let adj = vec![Adjustment {
            symbol: "AAAA".into(),
            panic_score: 90.0,
            reason: "je préfère 90".into(),
        }];
        assert_eq!(apply_adjustments(&mut rows, &adj), 0);
        assert_eq!(rows[0].panic_score, Some(55.0));
        assert!(rows[0].adjustment_reason.is_none());
    }

    /// An unjustified adjustment is exactly the "spread the notes" behaviour the
    /// rubric forbids, so it never reaches `apply_adjustments`.
    #[test]
    fn an_adjustment_without_a_reason_is_dropped_at_parse_time() {
        let adj = parse_calibration(r#"{"adjustments":[{"symbol":"AAAA","panic_score":60}]}"#);
        assert!(adj.is_empty());
        let blank =
            parse_calibration(r#"{"adjustments":[{"symbol":"AAAA","panic_score":60,"reason":"  "}]}"#);
        assert!(blank.is_empty());
    }

    #[test]
    fn an_adjustment_for_an_unknown_ticker_is_ignored() {
        let mut rows = rows_for_calibration();
        let adj = vec![Adjustment {
            symbol: "ZZZZ".into(),
            panic_score: 50.0,
            reason: "inventé".into(),
        }];
        assert_eq!(apply_adjustments(&mut rows, &adj), 0);
    }

    /// Losing calibration must never cost a completed run its scores.
    #[test]
    fn an_unparseable_calibration_reply_changes_nothing() {
        assert!(parse_calibration("").is_empty());
        assert!(parse_calibration("désolé, je ne peux pas").is_empty());
        assert!(parse_calibration(r#"{"adjustments":"nope"}"#).is_empty());
        assert!(parse_calibration(r#"{}"#).is_empty());
        // The explicit "nothing to change" answer is the normal, expected outcome.
        assert!(parse_calibration(r#"{"adjustments":[]}"#).is_empty());
    }

    #[test]
    fn adjustment_symbols_are_matched_case_insensitively() {
        let mut rows = rows_for_calibration();
        let adj = parse_calibration(
            r#"{"adjustments":[{"symbol":"aaaa","panic_score":60,"reason":"cohérence"}]}"#,
        );
        assert_eq!(adj[0].symbol, "AAAA");
        assert_eq!(apply_adjustments(&mut rows, &adj), 1);
    }
}
