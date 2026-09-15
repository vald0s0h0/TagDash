// Live company intel for the A+ qualification.
//
// WHY NOT JUST READ THE DB: `company_intel` refreshes on a TTL across the whole
// universe, so a given ticker's filings and financials can be days old. That is fine
// for a reporting table and not fine for a decision: an offering priced this morning
// is exactly the fact that should stop an alert, and it is exactly the fact a stale
// cache does not have.
//
// So this module fetches at qualification time and falls back to the cache — never
// the reverse — and records which happened per field group, so the rubric can tell a
// fresh filing list from last week's (`data_quality.sources`).
//
// IT DOES NOT REIMPLEMENT THE PROVIDERS. `company_intel::refresh_company_intel`
// already fans out to Massive (short interest), SEC EDGAR (filings, financials) and
// FMP (financials fallback), with each provider's rate limiter, retry policy and
// error taxonomy — and it writes what it finds to the DB. Calling it under a timeout
// and then reading the DB back gives fresh data when it lands and cached data when it
// doesn't, with one code path for both and no second set of HTTP clients to keep in
// sync. The refresh also warms the cache for everyone else.
//
// BUDGETS. The news pass is the one the trader waits on, so it gets a short budget
// and only the fast per-ticker endpoints. The deep pass runs after the alert is
// already out (or while it is still blocked, in blocking mode) and can afford SEC.
//
// RATE LIMITS. The SEC publishes a ~10 req/s fair-use policy and FMP's free tier is
// tight. Live intel is gathered at most ONCE per symbol per session (`quality` holds
// the result for the re-scores), and the qualification semaphore caps concurrency.

use std::sync::{Arc, Mutex, RwLock};

use chrono::Utc;
use tokio::time::{timeout, Duration};

use crate::company_intel::model::{CompanyIntel, SecFiling};
use crate::config::secrets::Secrets;
use crate::config::AppConfig;
use crate::local_db::{cache_repository, company_intel_repository, company_meta_repository};
use crate::quality_llm::payload::Source;

/// Budget for the news pass — short, because the alert is waiting on it.
pub const NEWS_BUDGET: Duration = Duration::from_millis(2_500);
/// Budget for the deep pass.
pub const DEEP_BUDGET: Duration = Duration::from_secs(8);
/// How far back the REST news sweep reaches. Wider than the websocket's in-memory
/// retention on purpose: a headline published at 03:50 while the app started at 04:10
/// never reached `MarketState`, and it is often the whole reason the ticker moved.
pub const NEWS_LOOKBACK_DAYS: i64 = 2;
const NEWS_LIMIT: u32 = 10;

/// Which network work a qualification phase actually needs. The DB dossier is read
/// on every call, but live endpoints are deliberately scoped so the deep pass does
/// not refetch the same news/float and a news re-score does not hit Massive again.
#[derive(Debug, Clone, Copy)]
pub struct GatherPlan {
    pub fetch_news: bool,
    pub fetch_float: bool,
    pub refresh_company_intel: bool,
}

impl GatherPlan {
    pub const NEWS: Self = Self {
        fetch_news: true,
        fetch_float: true,
        refresh_company_intel: false,
    };
    /// The only plan that spends provider calls on the DOSSIER — SEC filings,
    /// financials, dilution. It is requested exclusively by the deep pass, so with
    /// `quality_aplus` off no filing or statement is ever fetched for a candidate: the
    /// news rubric grades the catalyst and would not read them anyway.
    /// `no_dossier_work_without_the_deep_pass` pins that.
    pub const DEEP: Self = Self {
        fetch_news: false,
        fetch_float: false,
        refresh_company_intel: true,
    };
    pub const NEWS_ONLY: Self = Self {
        fetch_news: true,
        fetch_float: false,
        refresh_company_intel: false,
    };
    pub const CACHE_ONLY: Self = Self {
        fetch_news: false,
        fetch_float: false,
        refresh_company_intel: false,
    };
}

/// One ticker's dossier, with the provenance of each field group.
#[derive(Debug, Clone, Default)]
pub struct LiveIntel {
    pub intel: Option<CompanyIntel>,
    pub filings: Vec<SecFiling>,
    pub fundamental: Option<cache_repository::FundamentalCache>,
    pub risk: Option<cache_repository::RiskScores>,
    pub dilution_splits: Option<cache_repository::DilutionAndSplits>,
    pub meta: Option<company_meta_repository::CompanyMeta>,
    pub news: Vec<crate::alpaca::news::NewsArticle>,
    /// Per field group, matching `payload::SOURCE_GROUPS`.
    pub float_and_short_source: Source,
    pub news_source: Source,
    pub risk_source: Source,
    pub filings_source: Source,
    pub financials_source: Source,
}

impl LiveIntel {
    /// Cash ÷ quarterly burn, the figure that decides whether an ATM is an armed
    /// threat or a dormant facility. None when either half is unknown, or when the
    /// company isn't burning — a profitable issuer has no runway to run out of, and
    /// reporting a huge number there would read as a fact rather than an absence.
    pub fn cash_runway_quarters(&self) -> Option<f64> {
        let f = &self.intel.as_ref()?.financials;
        let cash = f.cash_and_equivalents?;
        // Quarterly burn from the TTM operating cash flow when it is negative;
        // otherwise from the TTM net loss. Both positive numbers when burning.
        let annual_burn = match f.operating_cash_flow_ttm {
            Some(ocf) if ocf < 0.0 => -ocf,
            _ => match f.net_income_ttm {
                Some(ni) if ni < 0.0 => -ni,
                _ => return None,
            },
        };
        let quarterly = annual_burn / 4.0;
        (quarterly > 0.0 && cash >= 0.0).then(|| cash / quarterly)
    }

    /// Whether an at-the-market facility is on file. None when we have no filing data
    /// at all — which must not be read as "no ATM".
    pub fn atm_active(&self) -> Option<bool> {
        let intel = self.intel.as_ref()?;
        if intel.dilution.is_empty() && self.filings.is_empty() {
            return None;
        }
        Some(intel.dilution.flags.atm || self.filings.iter().any(|f| f.detected_atm))
    }

    /// A priced offering filed within the last day. The single hard reject of the
    /// dilution gate: the issuer is selling stock into this move right now.
    pub fn offering_priced_within_hours(
        &self,
        hours: i64,
        as_of: chrono::DateTime<chrono::Utc>,
    ) -> bool {
        let cutoff = as_of - chrono::Duration::hours(hours);
        self.filings.iter().any(|f| {
            let priced = f.form_type.to_uppercase().starts_with("424B");
            priced
                && f.filing_date
                    .as_deref()
                    .and_then(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
                    // Filing cache has day precision. Never use a future trading
                    // day when the price feed/replay clock is behind wall time.
                    .map(|d| d >= cutoff.date_naive() && d <= as_of.date_naive())
                    .unwrap_or(false)
        })
    }
}

/// Gather the dossier for `symbol`, fetching what fits inside `budget` and falling
/// back to the cache for the rest.
///
/// `plan` selects only the live work this phase needs. On the news pass SEC/FMP is
/// skipped; on the deep pass news and float are reused from it; on a headline
/// re-score only the REST news sweep is refreshed.
pub async fn gather(
    db: &Arc<Mutex<rusqlite::Connection>>,
    config: &Arc<RwLock<AppConfig>>,
    secrets: &Arc<RwLock<Secrets>>,
    symbol: &str,
    budget: Duration,
    plan: GatherPlan,
) -> LiveIntel {
    // During a Market Replay, an UNDATED provider answer is a look-ahead leak: float,
    // short interest, filings and financials all come back as they stand TODAY, for a
    // session being replayed from the past. Those stay off; the DB dossier read below
    // is date-bounded and is the only honest source for them.
    //
    // NEWS IS THE EXCEPTION, and deliberately so. Both news fetchers cap their window
    // at the simulated instant — `alpaca::news::fetch_recent_news` adds `end = now`
    // and `massive::news::fetch_latest_news` adds `published_utc.lte = now`, both
    // reading the replay clock through `crate::time::now()` — so a headline published
    // later in the replayed day cannot come back. Blocking them bought no safety and
    // cost the whole point of replaying Dislocation: the news third is a CATALYST
    // decision, and
    // with the news half forced to `Missing` every candidate was scored on an absence
    // the live run would never have had. The websocket store fed by the replayed tape
    // stays merged in on top (see `quality::build_news`) — REST reaches back before
    // the replay's own start time, which the in-memory store cannot.
    let replay = crate::replay::clock::is_active();

    let (alpaca, massive_key) = {
        let s = secrets.read().unwrap();
        (
            s.alpaca_key.clone().zip(s.alpaca_secret.clone()),
            s.massive_api_key.clone(),
        )
    };

    // ── Fetches, in parallel, each capped by the shared budget ──
    let news_fetch = async {
        if !plan.fetch_news {
            return None;
        }
        if let Some((key, secret)) = alpaca.as_ref() {
            if let Some(news) = timeout(
                budget,
                crate::alpaca::news::fetch_recent_news(
                    key,
                    secret,
                    symbol,
                    NEWS_LOOKBACK_DAYS,
                    NEWS_LIMIT,
                ),
            )
            .await
            .ok()
            .and_then(Result::ok)
            .filter(|items| !items.is_empty())
            {
                return Some(news);
            }
        }

        // Massive Starter can be the only configured market provider. Its official
        // reference-news endpoint is available on every Stocks plan (hourly
        // recency), so A+ still receives catalyst context without Alpaca keys —
        // in a replay as in live, since that endpoint clamps itself to the
        // simulated instant.
        let key = massive_key.as_ref()?;
        let items = timeout(budget, crate::massive::news::fetch_latest_news(key, symbol))
            .await
            .ok()?
            .ok()?;
        let converted = items
            .into_iter()
            .filter_map(|item| {
                let created_at = item
                    .published_at
                    .as_deref()
                    .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())?
                    .with_timezone(&Utc);
                Some(crate::alpaca::news::NewsArticle {
                    created_at,
                    headline: item.title,
                    summary: item.summary.unwrap_or_default(),
                    content: String::new(),
                    source: item.source.unwrap_or_else(|| "Massive".into()),
                })
            })
            .collect::<Vec<_>>();
        (!converted.is_empty()).then_some(converted)
    };

    let float_fetch = async {
        if replay || !plan.fetch_float {
            return None;
        }
        let key = massive_key.as_ref()?;
        timeout(budget, crate::massive::fetch_float(key, symbol))
            .await
            .ok()?
            .ok()
            .flatten()
    };

    let intel_fetch = async {
        if replay || !plan.refresh_company_intel {
            return false;
        }
        // Writes straight to the DB; we read it back below either way, so the return
        // value is only "did fresh data land in time".
        timeout(
            budget,
            crate::company_intel::refresh_company_intel(
                db.clone(),
                config.clone(),
                secrets.clone(),
                symbol.to_string(),
            ),
        )
        .await
        .map(|summary| summary.succeeded > 0)
        .unwrap_or(false)
    };

    let (news, float_live, intel_live) = tokio::join!(news_fetch, float_fetch, intel_fetch);

    // The live float is worth persisting: it is the freshest the app will have, and
    // gate 1 reads the same table on its next refresh.
    if let Some(f) = &float_live {
        if let Ok(conn) = db.lock() {
            let existing = cache_repository::get_fundamental(&conn, symbol)
                .ok()
                .flatten();
            let _ = cache_repository::upsert_fundamental(
                &conn,
                &cache_repository::FundamentalCache {
                    symbol: symbol.to_string(),
                    float_shares: (f.float_shares > 0.0).then_some(f.float_shares as i64),
                    outstanding_shares: (f.outstanding_shares > 0.0)
                        .then_some(f.outstanding_shares as i64),
                    free_float: (f.free_float > 0.0).then_some(f.free_float),
                    prev_close: existing.as_ref().and_then(|e| e.prev_close),
                    avg_volume: existing.as_ref().and_then(|e| e.avg_volume),
                    atr: existing.as_ref().and_then(|e| e.atr),
                    updated_at: Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
                },
            );
        }
    }

    // ── Read the whole dossier back from the DB in one short lock ──
    let day = crate::time::et_date(crate::time::now());
    let (intel, filings, fundamental, risk, dilution_splits, meta) = {
        let conn = match db.lock() {
            Ok(c) => c,
            Err(_) => return LiveIntel::default(),
        };
        (
            company_intel_repository::get_intel(&conn, symbol)
                .ok()
                .flatten(),
            company_intel_repository::get_recent_filings_before(&conn, symbol, &day, 12)
                .unwrap_or_default(),
            cache_repository::get_fundamental(&conn, symbol)
                .ok()
                .flatten(),
            cache_repository::get_risk_scores(&conn, symbol).ok(),
            cache_repository::get_dilution_and_splits(&conn, symbol).ok(),
            company_meta_repository::get_by_symbol(&conn, symbol)
                .ok()
                .flatten(),
        )
    };

    // Provenance. A group is `Live` only when a fetch actually landed inside the
    // budget; otherwise `Db` when the cache has something, else `Missing`. Claiming
    // `Live` for a value that came from the cache is the one thing that would make
    // `data_quality` worse than useless.
    let has_intel = intel.is_some();
    LiveIntel {
        float_and_short_source: match (float_live.is_some(), fundamental.is_some()) {
            (true, _) => Source::Live,
            (false, true) => Source::Db,
            (false, false) => Source::Missing,
        },
        // No `Db` arm: there is no news cache to fall back on. Either the REST sweep
        // landed inside the budget or this dossier carries no news of its own — the
        // caller still merges whatever the websocket/replay store holds.
        news_source: if news.is_some() {
            Source::Live
        } else {
            Source::Missing
        },
        risk_source: if risk.is_some() {
            Source::Db
        } else {
            Source::Missing
        },
        filings_source: match (intel_live && !filings.is_empty(), filings.is_empty()) {
            (true, _) => Source::Live,
            (false, false) => Source::Db,
            (false, true) => Source::Missing,
        },
        financials_source: match (intel_live && has_intel, has_intel) {
            (true, _) => Source::Live,
            (false, true) => Source::Db,
            (false, false) => Source::Missing,
        },
        intel,
        filings,
        fundamental,
        risk,
        dilution_splits,
        meta,
        news: news.unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::company_intel::model::{DilutionFlags, DilutionInfo, FinancialHealth};

    fn intel_with(financials: FinancialHealth, dilution: DilutionInfo) -> LiveIntel {
        LiveIntel {
            intel: Some(CompanyIntel {
                symbol: "TEST".into(),
                financials,
                dilution,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn burning(cash: f64, ocf_ttm: f64) -> FinancialHealth {
        FinancialHealth {
            cash_and_equivalents: Some(cash),
            operating_cash_flow_ttm: Some(ocf_ttm),
            ..Default::default()
        }
    }

    #[test]
    fn runway_is_cash_over_quarterly_burn() {
        // $21M cash, $14M burned over twelve months → $3.5M a quarter → 6 quarters.
        let li = intel_with(
            burning(21_000_000.0, -14_000_000.0),
            DilutionInfo::default(),
        );
        assert_eq!(li.cash_runway_quarters(), Some(6.0));
    }

    #[test]
    fn a_profitable_issuer_has_no_runway_to_report() {
        // Not "infinite runway" — the concept doesn't apply, and a huge number would
        // read to the model as a measured fact.
        let li = intel_with(burning(21_000_000.0, 8_000_000.0), DilutionInfo::default());
        assert_eq!(li.cash_runway_quarters(), None);
    }

    #[test]
    fn net_loss_stands_in_when_cash_flow_is_unknown() {
        let fh = FinancialHealth {
            cash_and_equivalents: Some(10_000_000.0),
            operating_cash_flow_ttm: None,
            net_income_ttm: Some(-20_000_000.0),
            ..Default::default()
        };
        assert_eq!(
            intel_with(fh, DilutionInfo::default()).cash_runway_quarters(),
            Some(2.0)
        );
    }

    #[test]
    fn missing_financials_are_absent_not_zero() {
        assert_eq!(LiveIntel::default().cash_runway_quarters(), None);
        let li = intel_with(FinancialHealth::default(), DilutionInfo::default());
        assert_eq!(li.cash_runway_quarters(), None);
    }

    /// "We have no filing data" and "there is no ATM" are different answers, and the
    /// score penalty must only fire on the second.
    #[test]
    fn an_unknown_atm_is_none_not_false() {
        assert_eq!(LiveIntel::default().atm_active(), None);
        let li = intel_with(FinancialHealth::default(), DilutionInfo::default());
        assert_eq!(
            li.atm_active(),
            None,
            "an empty dilution record proves nothing"
        );

        let known_clean = intel_with(
            FinancialHealth::default(),
            DilutionInfo {
                has_recent_shelf: true,
                ..Default::default()
            },
        );
        assert_eq!(known_clean.atm_active(), Some(false));

        let armed = intel_with(
            FinancialHealth::default(),
            DilutionInfo {
                has_recent_shelf: true,
                flags: DilutionFlags {
                    atm: true,
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        assert_eq!(armed.atm_active(), Some(true));
    }

    #[test]
    fn a_future_filing_day_cannot_veto_a_delayed_market_event() {
        let as_of = chrono::DateTime::parse_from_rfc3339("2026-08-20T08:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let priced = |day: &str| SecFiling {
            form_type: "424B5".into(),
            filing_date: Some(day.into()),
            ..Default::default()
        };
        let current = LiveIntel {
            filings: vec![priced("2026-08-20")],
            ..Default::default()
        };
        assert!(current.offering_priced_within_hours(24, as_of));

        let future = LiveIntel {
            filings: vec![priced("2026-08-21")],
            ..Default::default()
        };
        assert!(!future.offering_priced_within_hours(24, as_of));
    }
}
