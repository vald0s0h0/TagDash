// The standardized per-ticker payload handed to the model.
//
// ONE INVARIANT GOVERNS THIS FILE: every ticker's payload carries EXACTLY the same
// keys, in the same order, whatever the data situation. Missing values are explicit
// `null`s plus an entry in `data_quality.missing`, never absent keys.
//
// That is not tidiness, it is the precondition for a comparable score. If a
// data-rich ticker's payload had ten sections and a data-poor one had six, the model
// would read the difference as a signal about the TICKER rather than about our
// coverage, and would systematically mis-rank thin names. Serde emits `Option::None`
// as `null` for a named field, so keeping every field declared is what enforces the
// invariant — `skip_serializing_if` must never be added here. The
// `payload_shape_is_identical_when_everything_is_missing` test guards it.
//
// The second job of this file is HONESTY about provenance:
//   • `data_quality.feed` says IEX or SIP, and `iex_only` lists the fields that are
//     a single venue's view rather than the consolidated tape — an IEX RVOL is not a
//     market RVOL and the rubric tells the model to discount it accordingly.
//   • `data_quality.lookahead` lists fields that are CURRENT snapshots rather than
//     as-of-the-replayed-day values (float, market cap, short interest, risk scores).
//     They cannot be reconstructed historically without versioned fundamentals, so
//     during a replay we declare the leak instead of pretending it is not there.
//
// Everything here is read from stores the Startup Pipeline already fills; no new bulk
// collection is introduced. The derived metrics are pure functions over bar slices so
// they can be tested without a database or a network.

use serde::Serialize;

use crate::market_state::aggregators::Bar;

/// Daily bars pulled for the context block (enough for the multi-day view the
/// strategy reasons over, without bloating the prompt).
pub const DAILY_BARS: u32 = 10;
/// Intraday minute bars considered for the momentum metrics.
pub const INTRADAY_LOOKBACK_MIN: usize = 30;
/// Cap on news items and on each item's body, mirroring the existing panic read.
pub const NEWS_LIMIT: u32 = 10;
pub const NEWS_DAYS: i64 = 5;
pub const NEWS_DAYS_MAX: i64 = 14;
pub const NEWS_BODY_CHARS: usize = 700;
/// SEC filings included per ticker (newest first, date-bounded).
pub const FILINGS_LIMIT: u32 = 12;

// ─── Sections ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize)]
pub struct Identity {
    pub symbol: String,
    pub name: Option<String>,
    pub exchange: Option<String>,
    pub security_type: Option<String>,
    pub country: Option<String>,
    pub industry: Option<String>,
    pub sector: Option<String>,
    pub sic: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Price {
    pub last: Option<f64>,
    pub prev_close: Option<f64>,
    pub open_today: Option<f64>,
    pub change_from_prev_close_pct: Option<f64>,
    pub change_from_open_pct: Option<f64>,
    pub change_1m_pct: Option<f64>,
    pub change_5m_pct: Option<f64>,
    pub change_15m_pct: Option<f64>,
    pub hod: Option<f64>,
    pub lod: Option<f64>,
    pub vwap: Option<f64>,
    pub distance_to_vwap_pct: Option<f64>,
    /// Bollinger Z of the live price on a 20-day daily basis.
    pub bollinger_z: Option<f64>,
    /// Close-to-close change over 1..6 trading days (`fundamentals_cache`).
    pub change_multiday_pct: [Option<f64>; 6],
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Volume {
    pub volume_since_open: Option<i64>,
    pub volume_1m: Option<i64>,
    pub volume_5m: Option<i64>,
    pub dollar_volume_today: Option<f64>,
    pub avg_daily_volume: Option<i64>,
    pub rvol_intraday: Option<f64>,
    /// Today's cumulative volume at this time of day vs the same clock position on
    /// prior sessions — catches "huge for 07:10" that a whole-day RVOL misses.
    pub rvol_time_of_day: Option<f64>,
    /// Last 5 minutes of volume vs the 5 before them.
    pub volume_acceleration: Option<f64>,
    /// Trades per minute over the recent window (SIP only in practice).
    pub trade_frequency_per_min: Option<f64>,
    pub premarket_volume: Option<i64>,
    pub premarket_dollar_volume: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Volatility {
    pub atr20: Option<f64>,
    pub atr_pct_of_price: Option<f64>,
    /// Today's high-low range divided by ATR20.
    pub range_over_atr: Option<f64>,
    /// |net move| / sum(|per-bar moves|) over the window: 1.0 = a straight line,
    /// near 0 = chop. A high value on a large move is the signature of a one-way
    /// forced flow rather than two-sided trading.
    pub directional_efficiency: Option<f64>,
    /// Deepest counter-trend retracement within the window, in percent.
    pub max_pullback_pct: Option<f64>,
    /// Recent per-minute move vs the earlier per-minute move (>1 = accelerating).
    pub price_acceleration: Option<f64>,
    /// Consecutive same-direction minute bars ending at the last bar. Signed:
    /// positive = up, negative = down. The strategy's "barres consécutives" factor.
    pub consecutive_bars: Option<i32>,
    /// Consecutive same-direction DAILY closes, signed the same way.
    pub consecutive_days: Option<i32>,
}

/// TagDash's own quantitative panic reading — the metric that SELECTED this ticker.
/// Given to the model as evidence, explicitly NOT as the answer: it measures how
/// statistically exceptional the move is, not whether it is justified.
#[derive(Debug, Clone, Default, Serialize)]
pub struct QuantPanic {
    pub score: Option<f64>,
    pub sigma: Option<f64>,
    pub atr_expansion: Option<f64>,
    pub window_days: Option<u8>,
    /// +1 up, −1 down.
    pub direction: Option<i8>,
    pub cap_bin: Option<String>,
    /// multiday | premarket.
    pub setup_kind: Option<String>,
    pub move_pct: Option<f64>,
    pub stability_score: Option<f64>,
    pub volume_expansion: Option<f64>,
    pub directional_efficiency: Option<f64>,
    pub consecutive_bars: Option<i32>,
    /// Calendar day on which the winning movement started.
    pub movement_start: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Microstructure {
    pub bid: Option<f64>,
    pub ask: Option<f64>,
    pub spread: Option<f64>,
    pub spread_pct: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct FloatAndShort {
    pub float_shares: Option<i64>,
    pub shares_outstanding: Option<i64>,
    pub free_float_pct: Option<f64>,
    pub market_cap: Option<f64>,
    pub short_interest: Option<i64>,
    pub short_interest_pct_float: Option<f64>,
    pub days_to_cover: Option<f64>,
    pub short_interest_settlement: Option<String>,
    pub shortable: Option<bool>,
    /// Not collected (Massive exposes no implemented endpoint) — declared so the key
    /// set stays identical and the model is told it is unavailable, not zero.
    pub short_volume: Option<i64>,
    pub short_volume_ratio: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct RiskScores {
    pub dilution_capacity_score: Option<f64>,
    pub dilution_need_score: Option<f64>,
    pub short_interest_score: Option<f64>,
    pub dilution_score: Option<f64>,
    pub dilution_pct_12m: Option<f64>,
    pub pump_dump_score: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Filing {
    pub form_type: String,
    pub filing_date: Option<String>,
    pub category: String,
    pub description: Option<String>,
    pub atm: bool,
    pub resale: bool,
    pub warrants: bool,
    pub offering_amount: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SecSection {
    pub has_recent_shelf: bool,
    pub latest_dilution_form: Option<String>,
    pub latest_dilution_date: Option<String>,
    pub recent_filings: Vec<Filing>,
    pub last_split_date: Option<String>,
    pub last_split_label: Option<String>,
    pub splits_last_1y: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Financials {
    pub net_income_last_q: Option<f64>,
    pub net_income_ttm: Option<f64>,
    pub negative_quarters_last4: Option<i64>,
    pub operating_cash_flow_ttm: Option<f64>,
    pub cash_and_equivalents: Option<f64>,
    pub period_end: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct NewsItem {
    pub published_at: String,
    pub hours_ago: Option<f64>,
    pub headline: String,
    pub source: Option<String>,
    pub body_excerpt: Option<String>,
}

/// What the context cascade produced. `stage` names the deepest source consulted, so
/// the model always knows whether "no catalyst" means "we looked everywhere" or
/// "we could not look".
#[derive(Debug, Clone, Default, Serialize)]
pub struct ContextSources {
    /// alpaca_news | massive_news | web | aucune
    pub stage: String,
    pub alpaca_news_count: usize,
    pub massive_news_count: usize,
    pub web_used: bool,
    pub web_provider: Option<String>,
    /// Why the web step did not run (replay, no key, failure) — None when it did.
    pub web_skipped_reason: Option<String>,
    pub web_answer: Option<String>,
    pub web_hits: Vec<WebHit>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct WebHit {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct DataQuality {
    /// "iex" (free) | "sip" (paid) | "inconnu".
    pub feed: String,
    /// Fields that reflect one venue only rather than the consolidated tape.
    pub iex_only: Vec<String>,
    /// Fields we could not fill at all.
    pub missing: Vec<String>,
    /// Fields that are a CURRENT snapshot and therefore carry look-ahead in replay.
    pub lookahead: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct RunContext {
    /// "live" | "replay".
    pub mode: String,
    /// ET timestamp the analysis is anchored to (simulated instant in replay).
    pub as_of: String,
    pub trading_day: String,
    /// pre_open | premarket | open | after_hours (app session model).
    pub session: String,
    /// True whenever un-historised snapshots are in play (see `lookahead`).
    pub snapshot_is_current: bool,
}

/// One ticker's complete, standardized payload.
#[derive(Debug, Clone, Default, Serialize)]
pub struct TickerPayload {
    pub identity: Identity,
    pub price: Price,
    pub volume: Volume,
    pub volatility: Volatility,
    pub quant_panic: QuantPanic,
    pub microstructure: Microstructure,
    pub float_and_short: FloatAndShort,
    pub risk_scores: RiskScores,
    pub sec: SecSection,
    pub financials: Financials,
    pub news: Vec<NewsItem>,
    pub context_sources: ContextSources,
    pub data_quality: DataQuality,
    pub context: RunContext,
}

impl TickerPayload {
    /// Compact JSON for the prompt. Compact rather than pretty: the shape is fully
    /// described by the rubric, and pretty-printing would roughly double the token
    /// count for no gain in the model's comprehension.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string())
    }
}

// ─── Derived metrics (pure, testable) ──────────────────────────────────────────

/// Percent change between two prices; None on a non-positive or non-finite base.
pub fn pct_change(from: f64, to: f64) -> Option<f64> {
    if !from.is_finite() || !to.is_finite() || from <= 0.0 {
        return None;
    }
    let v = (to - from) / from * 100.0;
    v.is_finite().then_some(v)
}

/// Change over the last `minutes` of the (ascending) minute series: the close of the
/// final bar versus the close of the bar `minutes` back.
pub fn change_over_minutes(bars: &[Bar], minutes: usize) -> Option<f64> {
    if minutes == 0 || bars.len() < minutes + 1 {
        return None;
    }
    let last = bars.last()?.close;
    let base = bars[bars.len() - 1 - minutes].close;
    pct_change(base, last)
}

/// Total share volume over the last `minutes` bars.
pub fn volume_over_minutes(bars: &[Bar], minutes: usize) -> Option<i64> {
    if minutes == 0 || bars.is_empty() {
        return None;
    }
    let take = minutes.min(bars.len());
    Some(
        bars[bars.len() - take..]
            .iter()
            .map(|b| b.volume as i64)
            .sum(),
    )
}

/// Volume of the last `window` bars divided by that of the `window` before them.
/// None when there is not a full pair of windows, or when the earlier window is
/// empty — an infinite acceleration is not a number the model can rank.
pub fn volume_acceleration(bars: &[Bar], window: usize) -> Option<f64> {
    if window == 0 || bars.len() < window * 2 {
        return None;
    }
    let n = bars.len();
    let recent: u64 = bars[n - window..].iter().map(|b| b.volume).sum();
    let prior: u64 = bars[n - window * 2..n - window].iter().map(|b| b.volume).sum();
    if prior == 0 {
        return None;
    }
    Some(recent as f64 / prior as f64)
}

/// |absolute move| per minute over the last `window` bars, divided by the same
/// figure over the preceding `window`. >1 means the move is speeding up — the
/// strategy's single most important condition.
pub fn price_acceleration(bars: &[Bar], window: usize) -> Option<f64> {
    if window == 0 || bars.len() < window * 2 + 1 {
        return None;
    }
    let n = bars.len();
    let recent = pct_change(bars[n - 1 - window].close, bars[n - 1].close)?.abs();
    let prior = pct_change(bars[n - 1 - window * 2].close, bars[n - 1 - window].close)?.abs();
    if prior <= 0.0 {
        return None;
    }
    Some(recent / prior)
}

/// Directional efficiency over the window: |net move| / Σ|per-bar moves|, in 0..=1.
/// A value near 1 on a large move means one-way pressure (forced buying/selling);
/// near 0 means the same distance was covered by churning back and forth.
pub fn directional_efficiency(bars: &[Bar]) -> Option<f64> {
    if bars.len() < 2 {
        return None;
    }
    let net = (bars.last()?.close - bars.first()?.close).abs();
    let path: f64 = bars.windows(2).map(|w| (w[1].close - w[0].close).abs()).sum();
    if path <= 0.0 {
        return None;
    }
    let v = net / path;
    v.is_finite().then_some(v)
}

/// Deepest counter-trend retracement inside the window, in percent of the extreme.
/// For an up-move it is the largest drop from a running high; for a down-move the
/// largest rally from a running low. Shallow pullbacks on a big move are the
/// euphoria/capitulation signature the strategy looks for.
pub fn max_pullback_pct(bars: &[Bar]) -> Option<f64> {
    if bars.len() < 2 {
        return None;
    }
    let first = bars.first()?.close;
    let last = bars.last()?.close;
    let up = last >= first;
    let mut extreme = first;
    let mut worst = 0.0_f64;
    for b in bars {
        // ORDER MATTERS: measure against the extreme set by PRIOR bars, and only then
        // let this bar extend it. Updating first would compare a bar's low to its own
        // high, reporting a rising bar's ordinary range as a retracement — a 100→120
        // green bar would score as a 16.7% pullback when nothing retraced at all.
        if up {
            if extreme > 0.0 {
                worst = worst.max((extreme - b.low) / extreme * 100.0);
            }
            extreme = extreme.max(b.high);
        } else {
            if extreme > 0.0 {
                worst = worst.max((b.high - extreme) / extreme * 100.0);
            }
            extreme = extreme.min(b.low);
        }
    }
    worst.is_finite().then_some(worst)
}

/// Consecutive same-direction closes ending at the last element, signed (+up, −down).
/// Flat steps break the streak: a run of unchanged prints is not momentum.
pub fn consecutive_direction(closes: &[f64]) -> Option<i32> {
    if closes.len() < 2 {
        return None;
    }
    let mut it = closes.windows(2).rev();
    let first = it.next()?;
    let dir = match (first[1] - first[0]).partial_cmp(&0.0)? {
        std::cmp::Ordering::Greater => 1_i32,
        std::cmp::Ordering::Less => -1_i32,
        std::cmp::Ordering::Equal => return Some(0),
    };
    let mut run = 1_i32;
    for w in it {
        let step = w[1] - w[0];
        let same = if dir > 0 { step > 0.0 } else { step < 0.0 };
        if !same {
            break;
        }
        run += 1;
    }
    Some(run * dir)
}

/// Cumulative volume of `bars` up to `cutoff_minutes` ET, divided by the average of
/// the same clock-bounded cumulative volume on the `prior` sessions. This answers
/// "is this volume large FOR THIS TIME OF DAY", which a whole-session RVOL cannot:
/// pre-open, a day-volume RVOL is near zero for every ticker and ranks nothing.
pub fn rvol_time_of_day(today: i64, prior_same_time: &[i64]) -> Option<f64> {
    if today < 0 || prior_same_time.is_empty() {
        return None;
    }
    let usable: Vec<i64> = prior_same_time.iter().copied().filter(|v| *v > 0).collect();
    if usable.is_empty() {
        return None;
    }
    let avg = usable.iter().sum::<i64>() as f64 / usable.len() as f64;
    (avg > 0.0).then(|| today as f64 / avg)
}

/// Trades per minute over the window. `None` when the feed carries no trade counts
/// (the free IEX tier often does not) rather than reporting a misleading zero.
pub fn trade_frequency_per_min(bars: &[Bar]) -> Option<f64> {
    if bars.is_empty() {
        return None;
    }
    let counted: Vec<u64> = bars.iter().filter_map(|b| b.trade_count).collect();
    if counted.is_empty() {
        return None;
    }
    Some(counted.iter().sum::<u64>() as f64 / counted.len() as f64)
}

/// Strip HTML tags and collapse whitespace — news bodies arrive as markup.
/// Mirrors the helper in `crate::enrichment`, kept local so this module stays pure.
pub fn strip_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&amp;", "&")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.trim().to_string();
    }
    let cut: String = s.chars().take(max).collect();
    format!("{}…", cut.trim())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn bar(close: f64, volume: u64) -> Bar {
        ohlc(close, close, close, close, volume)
    }

    fn ohlc(open: f64, high: f64, low: f64, close: f64, volume: u64) -> Bar {
        Bar {
            time: Utc.timestamp_opt(1_700_000_000, 0).unwrap(),
            open,
            high,
            low,
            close,
            volume,
            vwap: None,
            trade_count: None,
        }
    }

    fn series(closes: &[f64]) -> Vec<Bar> {
        closes.iter().map(|c| bar(*c, 1_000)).collect()
    }

    /// THE invariant of this module. A payload built from nothing must serialise to
    /// the same key set as a fully-populated one, so the model cannot read our
    /// coverage gaps as a property of the ticker.
    #[test]
    fn payload_shape_is_identical_when_everything_is_missing() {
        fn keys(v: &serde_json::Value, path: &str, out: &mut Vec<String>) {
            match v {
                serde_json::Value::Object(m) => {
                    for (k, val) in m {
                        let p = format!("{path}.{k}");
                        out.push(p.clone());
                        keys(val, &p, out);
                    }
                }
                // Array CONTENTS legitimately vary (0 news vs 3 news); the array key
                // itself is already recorded by the branch above.
                _ => {}
            }
        }

        let empty = TickerPayload::default();
        let mut full = TickerPayload::default();
        full.identity.symbol = "MTEN".into();
        full.identity.name = Some("M-tron Industries".into());
        full.price.last = Some(12.5);
        full.price.change_multiday_pct = [Some(1.0), None, Some(3.0), None, None, Some(-2.0)];
        full.volume.rvol_intraday = Some(14.2);
        full.volatility.consecutive_bars = Some(-4);
        full.quant_panic.sigma = Some(4.2);
        full.float_and_short.float_shares = Some(4_000_000);
        full.risk_scores.pump_dump_score = Some(88.0);
        full.financials.net_income_ttm = Some(-1.2e7);
        full.news.push(NewsItem {
            published_at: "2026-08-14T11:00:00Z".into(),
            hours_ago: Some(2.0),
            headline: "Something happened".into(),
            source: Some("Benzinga".into()),
            body_excerpt: Some("body".into()),
        });
        full.sec.recent_filings.push(Filing {
            form_type: "424B5".into(),
            category: "dilution".into(),
            atm: true,
            ..Default::default()
        });

        let mut a = Vec::new();
        let mut b = Vec::new();
        keys(&serde_json::to_value(&empty).unwrap(), "", &mut a);
        keys(&serde_json::to_value(&full).unwrap(), "", &mut b);
        // The full payload's extra keys can only come from array ELEMENTS; strip
        // those and the two key sets must be identical.
        let a_set: std::collections::BTreeSet<_> = a.into_iter().collect();
        let b_set: std::collections::BTreeSet<_> = b
            .into_iter()
            .filter(|k| !k.starts_with(".news.") && !k.starts_with(".sec.recent_filings."))
            .collect();
        assert_eq!(
            a_set, b_set,
            "an empty payload must expose exactly the same keys as a full one",
        );
        // And every one of them must actually be present as an explicit null.
        let json = serde_json::to_string(&empty).unwrap();
        assert!(json.contains("\"float_shares\":null"));
        assert!(json.contains("\"rvol_time_of_day\":null"));
        assert!(json.contains("\"short_volume\":null"));
    }

    #[test]
    fn pct_change_refuses_a_degenerate_base() {
        assert_eq!(pct_change(10.0, 11.0), Some(10.0));
        assert_eq!(pct_change(0.0, 5.0), None);
        assert_eq!(pct_change(-1.0, 5.0), None);
        assert_eq!(pct_change(f64::NAN, 5.0), None);
    }

    #[test]
    fn windowed_change_and_volume_walk_back_from_the_last_bar() {
        let bars = series(&[10.0, 10.5, 11.0, 12.0]);
        // 1 minute back = the previous bar's close.
        assert_eq!(change_over_minutes(&bars, 1), pct_change(11.0, 12.0));
        assert_eq!(change_over_minutes(&bars, 3), pct_change(10.0, 12.0));
        // Not enough history is None, never a partial answer.
        assert_eq!(change_over_minutes(&bars, 4), None);
        assert_eq!(volume_over_minutes(&bars, 2), Some(2_000));
        // Asking for more than we have sums what exists (a cumulative total is still
        // meaningful) — unlike a change, which would silently shorten its window.
        assert_eq!(volume_over_minutes(&bars, 99), Some(4_000));
    }

    #[test]
    fn volume_acceleration_compares_two_equal_windows() {
        let bars = vec![bar(10.0, 100), bar(10.0, 100), bar(10.0, 400), bar(10.0, 400)];
        assert_eq!(volume_acceleration(&bars, 2), Some(4.0));
        // A silent earlier window would divide by zero → refuse instead of ∞.
        let zeros = vec![bar(10.0, 0), bar(10.0, 0), bar(10.0, 50), bar(10.0, 50)];
        assert_eq!(volume_acceleration(&zeros, 2), None);
        assert_eq!(volume_acceleration(&bars, 3), None, "needs two full windows");
    }

    #[test]
    fn price_acceleration_detects_a_speeding_move() {
        // +1% then +4% over equal windows.
        let bars = series(&[100.0, 100.5, 101.0, 103.0, 105.04]);
        let acc = price_acceleration(&bars, 2).expect("defined");
        assert!(acc > 3.0, "a sharply faster leg must read well above 1, got {acc}");
        let flat = series(&[100.0, 100.0, 100.0, 100.0, 100.0]);
        assert_eq!(price_acceleration(&flat, 2), None, "no earlier move to scale by");
    }

    #[test]
    fn directional_efficiency_separates_a_straight_line_from_chop() {
        let straight = series(&[10.0, 11.0, 12.0, 13.0]);
        assert!((directional_efficiency(&straight).unwrap() - 1.0).abs() < 1e-9);
        // Same endpoints, twice the path travelled.
        let choppy = series(&[10.0, 12.0, 11.0, 13.0]);
        let eff = directional_efficiency(&choppy).unwrap();
        assert!(eff < 0.7, "chop must score well below a clean drive, got {eff}");
        assert_eq!(directional_efficiency(&series(&[10.0])), None);
        assert_eq!(directional_efficiency(&series(&[10.0, 10.0])), None, "no path");
    }

    #[test]
    fn max_pullback_measures_against_the_running_extreme() {
        // Up-move that dips 10% off its high mid-way.
        let bars = vec![
            ohlc(100.0, 100.0, 100.0, 100.0, 10),
            ohlc(100.0, 120.0, 100.0, 120.0, 10),
            ohlc(120.0, 120.0, 108.0, 110.0, 10),
            ohlc(110.0, 130.0, 110.0, 130.0, 10),
        ];
        let pb = max_pullback_pct(&bars).expect("defined");
        assert!((pb - 10.0).abs() < 1e-6, "expected ~10%, got {pb}");

        // A move with no retracement at all reads 0, not None. This is the case that
        // catches the ordering bug: a single 100→110 green bar has a 100 low and a
        // 110 high, so comparing the low to its OWN high would invent a 9% pullback.
        let clean = vec![
            ohlc(100.0, 100.0, 100.0, 100.0, 10),
            ohlc(100.0, 110.0, 100.0, 110.0, 10),
        ];
        assert_eq!(max_pullback_pct(&clean), Some(0.0));

        // Symmetric for a down move: the largest RALLY off the running low.
        let down = vec![
            ohlc(100.0, 100.0, 100.0, 100.0, 10),
            ohlc(100.0, 100.0, 80.0, 80.0, 10),
            ohlc(80.0, 90.0, 80.0, 85.0, 10),
            ohlc(85.0, 85.0, 70.0, 70.0, 10),
        ];
        let rally = max_pullback_pct(&down).expect("defined");
        assert!(
            (rally - 12.5).abs() < 1e-6,
            "expected ~12.5% rally off the 80 low, got {rally}",
        );
    }

    #[test]
    fn consecutive_direction_is_signed_and_broken_by_a_flat_step() {
        assert_eq!(consecutive_direction(&[1.0, 2.0, 3.0, 4.0]), Some(3));
        assert_eq!(consecutive_direction(&[4.0, 3.0, 2.0, 1.0]), Some(-3));
        // Only the run ENDING at the last close counts.
        assert_eq!(consecutive_direction(&[5.0, 1.0, 2.0, 3.0]), Some(2));
        // A flat last step is neither up nor down.
        assert_eq!(consecutive_direction(&[1.0, 2.0, 2.0]), Some(0));
        assert_eq!(consecutive_direction(&[1.0]), None);
    }

    /// Pre-open, a whole-day RVOL is ~0 for everything; the time-of-day figure is
    /// what actually discriminates. Zero-volume prior sessions are ignored rather
    /// than dragging the average down.
    #[test]
    fn rvol_time_of_day_ignores_dead_prior_sessions() {
        assert_eq!(rvol_time_of_day(1_000, &[500, 500]), Some(2.0));
        assert_eq!(rvol_time_of_day(1_000, &[0, 0, 500]), Some(2.0));
        assert_eq!(rvol_time_of_day(1_000, &[0, 0]), None);
        assert_eq!(rvol_time_of_day(1_000, &[]), None);
    }

    /// The free tier often omits trade counts. Reporting 0 trades/min would read as
    /// "nobody is trading it", the opposite of the truth.
    #[test]
    fn trade_frequency_is_none_when_the_feed_omits_counts() {
        let bars = series(&[10.0, 11.0]);
        assert_eq!(trade_frequency_per_min(&bars), None);
        let mut with_counts = bars.clone();
        with_counts[0].trade_count = Some(10);
        with_counts[1].trade_count = Some(20);
        assert_eq!(trade_frequency_per_min(&with_counts), Some(15.0));
    }

    #[test]
    fn news_bodies_are_stripped_and_capped() {
        assert_eq!(
            strip_html("<p>Hello&nbsp;<b>world</b></p>\n\n  x"),
            "Hello world x"
        );
        assert_eq!(truncate("abcdef", 3), "abc…");
        assert_eq!(truncate("  ab  ", 10), "ab");
    }
}
