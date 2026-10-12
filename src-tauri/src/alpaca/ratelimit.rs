// One request budget for the whole Alpaca data API (data.alpaca.markets).
//
// Alpaca counts every data call against a single per-key budget: 200 requests a
// minute on the free Basic plan, 10,000 on Algo Trader Plus. Replay preparation,
// the review's SIP rebuild, chart history, news and corporate actions all draw on
// it, so they pace through ONE limiter instead of each guessing on its own and
// tripping 429s together.
//
// The budget follows the last probed plan (`set_plan`: the plan remembered from the
// previous run at launch, then the live feed's probe and the replay downloader's).
// Until one is known it stays on the free budget — too slow costs time, too fast
// costs 429s.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::LazyLock;

use crate::company_intel::rate_limit::RateLimiter;

/// A little under the documented 200/min, so bursts from other tools on the same
/// key don't push us over.
const BASIC_RPM: f64 = 190.0;
const PAID_RPM: f64 = 9_000.0;

static BASIC: LazyLock<RateLimiter> = LazyLock::new(|| RateLimiter::per_minute(BASIC_RPM));
static PAID: LazyLock<RateLimiter> = LazyLock::new(|| RateLimiter::per_minute(PAID_RPM));
static IS_PAID: AtomicBool = AtomicBool::new(false);
static REQUESTS: AtomicUsize = AtomicUsize::new(0);

/// Data-API requests made since launch (for logs and progress estimates).
pub fn requests() -> usize {
    REQUESTS.load(Ordering::Relaxed)
}

/// Record the probed plan so the budget matches the subscription.
pub fn set_plan(plan: super::plan::Plan) {
    IS_PAID.store(matches!(plan, super::plan::Plan::Sip), Ordering::Relaxed);
}

/// Whether the budget is the paid plan's.
pub fn is_paid() -> bool {
    IS_PAID.load(Ordering::Relaxed)
}

/// Requests per minute Alpaca documents for the current plan (what the UI quotes;
/// the limiter itself paces a little under it).
pub fn documented_rpm() -> u32 {
    if is_paid() {
        10_000
    } else {
        200
    }
}

/// Requests a bulk download keeps in flight. The limiter spaces request STARTS, so
/// a caller whose responses take longer than a slot leaves budget unused: measured
/// on the free plan, one sequential downloader of 10 000-row pages reaches 1.6
/// requests a second of the 3.2 allowed. Four in flight fill that budget (the
/// limiter still holds them to it); the paid plan's takes more.
pub fn bulk_concurrency() -> usize {
    if is_paid() {
        8
    } else {
        4
    }
}

/// A slot for a background bulk download. While a Market Replay runs, the
/// download leaves about a quarter of the budget to what the trader is looking at
/// (chart history, news): each of its slots is followed by a pause of a third of
/// one.
pub async fn acquire_bulk() {
    acquire().await;
    bulk_pause().await;
}

/// The pause itself, for a bulk request whose slot was taken elsewhere.
pub async fn bulk_pause() {
    if crate::replay::clock::is_active() {
        let slot = 60.0 / if is_paid() { PAID_RPM } else { BASIC_RPM };
        tokio::time::sleep(std::time::Duration::from_secs_f64(slot / 3.0)).await;
    }
}

/// Wait for a slot in the data-API budget. Call right before every request to
/// data.alpaca.markets (one call per page).
pub async fn acquire() {
    REQUESTS.fetch_add(1, Ordering::Relaxed);
    if IS_PAID.load(Ordering::Relaxed) {
        PAID.acquire().await
    } else {
        BASIC.acquire().await
    }
}
