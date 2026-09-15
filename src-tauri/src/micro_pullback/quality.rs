// Micro Pullback's own catalyst read — the fast News-only LLM pass, reused wholesale
// from Dislocation's plumbing (`dislocation::quality::{build_payload, issuer_profile,
// market_now, Deps, Candidate}`, `dislocation::intel_live::gather`) rather than
// re-derived: the payload describes the TICKER's live state, not anything specific to
// Dislocation's own three-thirds blend.
//
// NO BLENDING. Micro Pullback's engine already gates on the tape alone before a
// candidate ever reaches here (see `micro_pullback::mod`'s "NO MODEL IN THE PATH"
// header — this module is the opt-in button/checkbox that puts a model back in,
// deliberately, downstream of detection). The number this module returns IS the
// model's `quality_score`; there is no RVOL/attention third to average it with, and
// no deep second pass — that combination is what Dislocation's A+ is for.
//
// ONE CALL PER CANDIDATE. No 30-minute news watch, no re-score on a later headline.
// `IN_FLIGHT` blocks the one real double-call risk left: auto firing while a manual
// click for the same symbol is still in the air (or the reverse).

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

use chrono::{DateTime, Utc};

use super::options::MicroPullbackOptions;
use crate::dislocation::quality::{build_payload, issuer_profile, market_now, Candidate, Deps};
use crate::dislocation::{intel_live, score};
use crate::quality_llm::{self, Pass, QualityAnswer};

const NEWS_SCORE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// Two at a time: a burst of departures at the open must not fan out into a dozen
/// concurrent Deepseek calls, but this is a single fast pass, not Dislocation's
/// four-slot news+deep pipeline, so a smaller pool is enough.
static QUALIFY_SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);
static IN_FLIGHT: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
static RECENT_CALLS: OnceLock<Mutex<Vec<DateTime<Utc>>>> = OnceLock::new();

fn in_flight() -> &'static Mutex<HashSet<String>> {
    IN_FLIGHT.get_or_init(|| Mutex::new(HashSet::new()))
}

fn recent_calls() -> &'static Mutex<Vec<DateTime<Utc>>> {
    RECENT_CALLS.get_or_init(|| Mutex::new(Vec::new()))
}

/// Whether the rolling 5-minute AUTO budget still has room. A manual click never
/// checks this — a human pressing a button is its own budget.
fn budget_available(now: DateTime<Utc>, budget_per_5min: u32) -> bool {
    let mut calls = recent_calls().lock().unwrap();
    calls.retain(|t| (now - *t).num_seconds() < 300);
    calls.len() < budget_per_5min as usize
}

fn record_call(now: DateTime<Utc>) {
    recent_calls().lock().unwrap().push(now);
}

/// RAII guard: claims the symbol on construction, releases it on drop (every return
/// path, including an early `?`/`return Err`, releases automatically).
struct InFlightGuard(String);

impl InFlightGuard {
    /// `None` when another call for this symbol is already in flight.
    fn claim(symbol: &str) -> Option<Self> {
        in_flight()
            .lock()
            .unwrap()
            .insert(symbol.to_string())
            .then(|| InFlightGuard(symbol.to_string()))
    }
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        in_flight().lock().unwrap().remove(&self.0);
    }
}

/// Build the candidate snapshot the shared payload builder expects, from what Micro
/// Pullback's own detection already computed for this fire. `scores`/`news_age_minutes`
/// are dummies: `build_payload` never reads them (they exist only for Dislocation's own
/// blended verdict, which this module never calls).
pub fn candidate_from_fire(
    alert: crate::types::AlertSignal,
    tempo_secs: i64,
    price_return_pct: f64,
    volume_ratio: f64,
    trade_rate_ratio: Option<f64>,
    range_ratio: f64,
    tape_source: crate::types::TapeKind,
    tape_rate_per_sec: Option<f64>,
    tape_dollar_per_sec: f64,
    tape_active_seconds: Option<u32>,
    tape_window_secs: i64,
    detected_at: DateTime<Utc>,
) -> Candidate {
    Candidate {
        tempo_depth: alert
            .tempo_depth
            .clone()
            .unwrap_or_default()
            .into_iter()
            .map(i64::from)
            .collect(),
        alert,
        tempo_secs,
        price_return_pct,
        volume_ratio,
        trade_rate_ratio,
        range_ratio,
        tape_source,
        tape_rate_per_sec,
        tape_dollar_per_sec,
        tape_active_seconds,
        tape_window_secs,
        detected_at,
        scores: score::Scores {
            rvol: 0.0,
            attention: 0.0,
            news: None,
        },
        news_age_minutes: None,
    }
}

/// Run the News pass for one candidate and persist the result.
///
/// `auto` gates the rolling budget check only — both the auto path and an explicit
/// user click otherwise share the exact same call, the same in-flight guard and the
/// same persistence, so there is never a second code path to keep in sync.
pub async fn run_news_scoring(
    deps: Deps,
    cand: Candidate,
    opts: MicroPullbackOptions,
    auto: bool,
) -> Result<QualityAnswer, String> {
    let symbol = cand.alert.symbol.clone();
    let Some(_guard) = InFlightGuard::claim(&symbol) else {
        return Err("un scoring est déjà en cours pour ce symbole".to_string());
    };

    let now = market_now(&deps);
    if auto && !budget_available(now, opts.llm_budget_per_5min) {
        return Err("budget de scoring automatique atteint pour les 5 dernières minutes".into());
    }

    let _slot = QUALIFY_SLOTS
        .try_acquire()
        .map_err(|_| "file d'attente de scoring pleine".to_string())?;

    let api_key = deps.secrets.read().unwrap().deepseek_api_key.clone();
    let Some(api_key) = api_key.filter(|k| !k.trim().is_empty()) else {
        return Err("pas de clé Deepseek configurée".to_string());
    };

    let intel = intel_live::gather(
        &deps.db,
        &deps.config,
        &deps.secrets,
        &symbol,
        intel_live::NEWS_BUDGET,
        intel_live::GatherPlan::NEWS_ONLY,
    )
    .await;
    let profile = issuer_profile(&intel);
    let payload = build_payload(&deps, &cand, &intel, &profile);

    record_call(now);
    let result = tokio::time::timeout(
        NEWS_SCORE_TIMEOUT,
        quality_llm::score(&api_key, Pass::News, &symbol, &payload),
    )
    .await;

    let answer = match result {
        Ok(Ok(a)) => a,
        Ok(Err(e)) => return Err(e),
        Err(_) => return Err(format!("délai {} s dépassé", NEWS_SCORE_TIMEOUT.as_secs())),
    };

    if let Ok(conn) = deps.db.lock() {
        let day = crate::time::et_date(now);
        let session = crate::dislocation::quality::session_label(now);
        let simulated = crate::replay::clock::is_active();
        let _ = crate::local_db::micro_pullback_quality_repository::upsert(
            &conn, &symbol, &day, &session, simulated, &answer,
        );
    }

    Ok(answer)
}
