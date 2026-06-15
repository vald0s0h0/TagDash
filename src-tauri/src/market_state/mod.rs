/// RAM source of truth for all live market data.
/// Ring buffers hold closed candles per ticker/timeframe.
/// No DB access on the hot path — every write is a plain HashMap mutation.
pub mod aggregators;
pub mod latency;
pub mod mock_feed;
pub mod ring_buffer;

use std::collections::{HashMap, VecDeque};

use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

use crate::types::{LatencyLevel, LatencyStatus, NewsHeadline, NewsRef};
use self::aggregators::{Bar, CandleAggregator, Timeframe};
use self::ring_buffer::RingBuffer;

const RING_CAP: usize = 600;

/// NY trading-day bucket for a UTC instant (DST-aware). Daily candles are keyed
/// by this so one trading day is always one bar, regardless of the (DST-varying)
/// time-of-day Alpaca stamps its daily bars at.
fn ny_date(t: DateTime<Utc>) -> NaiveDate {
    crate::time::to_et(t).date_naive()
}

/// How long a live news headline stays on file for correlation (covers a whole
/// premarket session). Older entries are pruned on each new arrival.
const NEWS_RETENTION_SECS: i64 = 6 * 3600;
/// Cap on the flat news log surfaced in the debug panel (newest first).
const NEWS_LOG_CAP: usize = 100;
/// Cap on retained headlines per symbol (newest kept).
const NEWS_PER_SYMBOL_CAP: usize = 20;
/// Seconds of per-symbol trade-count history retained (≥ any accel window).
const TRADE_COUNT_RETAIN_SECS: i64 = 180;

/// Per-ticker live state — serialized and sent to the frontend via get_market_snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TickerLiveState {
    pub symbol:         String,
    pub last_price:     Option<f64>,
    pub bid:            Option<f64>,
    pub ask:            Option<f64>,
    pub spread:         Option<f64>,
    pub volume_day:     u64,
    pub vwap:           Option<f64>,
    pub high_day:       Option<f64>,
    pub low_day:        Option<f64>,
    pub previous_close: Option<f64>,
    pub change_day_pct: Option<f64>,
    pub latency_ui_ms:  Option<u32>,
    pub updated_at:     DateTime<Utc>,
}

impl TickerLiveState {
    /// A fresh ticker with only its symbol + timestamp set (all live fields
    /// empty). Used by the ingestion paths the first time a symbol is seen.
    fn new(symbol: &str, now: DateTime<Utc>) -> Self {
        Self {
            symbol:         symbol.to_string(),
            last_price:     None,
            bid:            None,
            ask:            None,
            spread:         None,
            volume_day:     0,
            vwap:           None,
            high_day:       None,
            low_day:        None,
            previous_close: None,
            change_day_pct: None,
            latency_ui_ms:  None,
            updated_at:     now,
        }
    }
}

/// Snapshot returned by get_market_snapshot (polled ~300 ms while feed is active).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketSnapshot {
    pub tickers:      HashMap<String, TickerLiveState>,
    pub latency:      LatencyStatus,
    pub mock_running: bool,
    /// True while the Alpaca live WebSocket feed is connected.
    pub live_running: bool,
}

/// Live health of the Alpaca WebSocket feed, surfaced in the diagnostics panel.
/// Updated by the stream task as it connects, authenticates, subscribes and
/// receives data — so the UI can tell at a glance whether streaming works.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedDiagnostics {
    /// idle | connecting | authenticating | authenticated | subscribed |
    /// streaming | error | reconnecting | stopped
    pub state:              String,
    pub feed:               String,         // iex | sip | delayed_sip
    /// Broad-tier channel for the whole universe: "trades" (premarket) | "bars" (open).
    pub broad_mode:         String,
    pub subscribed_symbols: usize,           // broad-tier symbol count
    /// Symbols currently tick-streamed (trades+quotes) because they're displayed.
    pub focus_symbols:      usize,
    /// Tickers dropped before subscribe because they aren't valid Alpaca symbols
    /// (a single malformed entry makes Alpaca reject the whole frame, code 400).
    pub invalid_symbols_dropped: usize,
    pub trades_received:    u64,
    pub quotes_received:    u64,
    pub bars_received:      u64,
    pub last_message_at:    Option<DateTime<Utc>>,
    pub last_error_code:    Option<i64>,
    pub last_error_msg:     Option<String>,
    /// The most recent subscribe/unsubscribe message sent (channel + chunk index
    /// + size). When an error follows, this reveals exactly which message Alpaca
    /// rejected — e.g. the symbol count at which a subscription cap is hit.
    pub last_subscribe:     String,
    /// What Alpaca actually confirms is subscribed, echoed back in its
    /// `subscription` control message (per-channel symbol counts + a sample).
    /// The ground truth: if the focus trades/quotes don't appear here, our
    /// subscribe didn't register the symbols (e.g. malformed/empty), even
    /// though no error was returned.
    pub subscription_ack:   String,
    pub reconnects:         u32,
    pub connected_at:       Option<DateTime<Utc>>,
    pub updated_at:         DateTime<Utc>,
}

impl Default for FeedDiagnostics {
    fn default() -> Self {
        Self {
            state:              "idle".into(),
            feed:               String::new(),
            broad_mode:         String::new(),
            subscribed_symbols: 0,
            focus_symbols:      0,
            invalid_symbols_dropped: 0,
            trades_received:    0,
            quotes_received:    0,
            bars_received:      0,
            last_message_at:    None,
            last_error_code:    None,
            last_error_msg:     None,
            last_subscribe:     String::new(),
            subscription_ack:   String::new(),
            reconnects:         0,
            connected_at:       None,
            updated_at:         Utc::now(),
        }
    }
}

/// Live health of the Alpaca news WebSocket feed (premarket news investor),
/// surfaced in the news debug panel so we can tell at a glance whether news
/// headlines are arriving correctly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewsDiagnostics {
    /// idle | waiting_premarket | connecting | authenticated | subscribed |
    /// streaming | error | stopped
    pub state:             String,
    /// True while inside the premarket window (the feed only streams then).
    pub in_premarket:      bool,
    pub news_received:     u64,
    pub symbols_with_news: usize,
    pub last_news_at:      Option<DateTime<Utc>>,
    pub last_headline:     Option<String>,
    pub last_symbols:      Vec<String>,
    pub last_error:        Option<String>,
    pub connected_at:      Option<DateTime<Utc>>,
    pub updated_at:        DateTime<Utc>,
    /// Newest-first flat log of recent headlines for the debug panel.
    pub recent:            Vec<NewsHeadline>,
}

impl Default for NewsDiagnostics {
    fn default() -> Self {
        Self {
            state:             "idle".into(),
            in_premarket:      false,
            news_received:     0,
            symbols_with_news: 0,
            last_news_at:      None,
            last_headline:     None,
            last_symbols:      Vec::new(),
            last_error:        None,
            connected_at:      None,
            updated_at:        Utc::now(),
            recent:            Vec::new(),
        }
    }
}

/// Per-symbol rolling count of trade prints, bucketed by unix second. Used to
/// measure trade acceleration (recent rate vs baseline rate) without storing
/// every print.
#[derive(Debug, Default)]
struct TradeCounter {
    /// (unix_second, count) buckets, oldest at the front, newest at the back.
    buckets: VecDeque<(i64, u32)>,
}

impl TradeCounter {
    /// Record `n` prints at `sec` (unix seconds), dropping buckets older than the
    /// retention window. Live trades record 1; replay slices record the share of
    /// the minute bar's trade count they carry.
    fn record(&mut self, sec: i64, n: u32) {
        match self.buckets.back_mut() {
            Some((s, c)) if *s == sec => *c += n,
            _ => self.buckets.push_back((sec, n)),
        }
        let cutoff = sec - TRADE_COUNT_RETAIN_SECS;
        while matches!(self.buckets.front(), Some((s, _)) if *s < cutoff) {
            self.buckets.pop_front();
        }
    }

    /// Number of prints in the last `secs` seconds relative to `now_sec`.
    fn count_last(&self, now_sec: i64, secs: i64) -> u64 {
        let cutoff = now_sec - secs;
        self.buckets
            .iter()
            .rev()
            .take_while(|(s, _)| *s > cutoff)
            .map(|(_, c)| *c as u64)
            .sum()
    }
}

/// Full in-process market state (not serialized — internal use only).
pub struct MarketState {
    pub tickers:      HashMap<String, TickerLiveState>,
    /// Per-ticker, per-timeframe candle aggregators (in-progress bars).
    aggregators:      HashMap<String, HashMap<Timeframe, CandleAggregator>>,
    /// Per-ticker, per-timeframe ring buffers of closed bars.
    pub bars:         HashMap<String, HashMap<Timeframe, RingBuffer<Bar>>>,
    pub latency:      LatencyStatus,
    pub mock_running: bool,
    /// True while the Alpaca live WebSocket feed is connected.
    pub live_running: bool,
    /// Live health of the Alpaca WebSocket feed (diagnostics panel).
    pub feed:         FeedDiagnostics,
    /// VWAP accumulators: (price×vol sum, vol sum) per symbol.
    vwap_acc:         HashMap<String, (f64, f64)>,
    /// Per-symbol rolling trade-print counters (trade acceleration measure).
    trade_counts:     HashMap<String, TradeCounter>,
    /// Live news headlines keyed by upper-cased symbol (correlation engine).
    news_by_symbol:   HashMap<String, Vec<NewsHeadline>>,
    /// Flat newest-first log of recent headlines (debug panel).
    news_log:         VecDeque<NewsHeadline>,
    /// Live health of the Alpaca news WebSocket feed (news debug panel).
    pub news_feed:    NewsDiagnostics,
}

impl MarketState {
    pub fn new() -> Self {
        Self {
            tickers:      HashMap::new(),
            aggregators:  HashMap::new(),
            bars:         HashMap::new(),
            latency: LatencyStatus {
                websocket_to_ui_ms: 0,
                level:              LatencyLevel::Normal,
                measured_at:        Utc::now(),
            },
            mock_running: false,
            live_running: false,
            feed:         FeedDiagnostics::default(),
            vwap_acc:     HashMap::new(),
            trade_counts:   HashMap::new(),
            news_by_symbol: HashMap::new(),
            news_log:       VecDeque::new(),
            news_feed:      NewsDiagnostics::default(),
        }
    }

    // ── Feed diagnostics updates (called by the live-stream task) ──────────────
    pub fn feed_set_feed(&mut self, feed: &str) {
        self.feed.feed = feed.to_string();
    }
    pub fn feed_state(&mut self, state: &str) {
        self.feed.state = state.to_string();
        self.feed.updated_at = Utc::now();
    }
    pub fn feed_authenticated(&mut self) {
        self.feed.state = "authenticated".to_string();
        self.feed.connected_at = Some(Utc::now());
        self.feed.updated_at = Utc::now();
    }
    /// Called after each subscription reconcile: records the broad-tier channel,
    /// the broad symbol count and the focus (tick-streamed) symbol count. Does not
    /// override a "streaming"/"error" state — only marks "subscribed" when idle.
    pub fn feed_reconcile(&mut self, broad_mode: &str, broad_count: usize, focus_count: usize) {
        self.feed.broad_mode = broad_mode.to_string();
        self.feed.subscribed_symbols = broad_count;
        self.feed.focus_symbols = focus_count;
        if self.feed.state == "authenticated" || self.feed.state == "connecting" {
            self.feed.state = "subscribed".to_string();
        }
        self.feed.updated_at = Utc::now();
    }
    pub fn feed_set_dropped(&mut self, dropped: usize) {
        self.feed.invalid_symbols_dropped = dropped;
    }
    pub fn feed_set_last_subscribe(&mut self, label: &str) {
        self.feed.last_subscribe = label.to_string();
    }
    pub fn feed_set_subscription_ack(&mut self, ack: &str) {
        self.feed.subscription_ack = ack.to_string();
        self.feed.updated_at = Utc::now();
    }
    pub fn feed_error(&mut self, code: i64, msg: Option<String>) {
        self.feed.state = "error".to_string();
        self.feed.last_error_code = Some(code);
        self.feed.last_error_msg = msg;
        self.feed.updated_at = Utc::now();
    }
    pub fn feed_reconnecting(&mut self) {
        self.feed.reconnects += 1;
        self.feed.state = "reconnecting".to_string();
        self.feed.updated_at = Utc::now();
    }
    pub fn feed_record_data(&mut self, trades: u64, quotes: u64, bars: u64, now: DateTime<Utc>) {
        self.feed.trades_received += trades;
        self.feed.quotes_received += quotes;
        self.feed.bars_received  += bars;
        self.feed.last_message_at = Some(now);
        if self.feed.state != "streaming" {
            self.feed.state = "streaming".to_string();
        }
        self.feed.updated_at = now;
    }

    /// Seed the previous (daily) close for a symbol so live `change_day_pct`
    /// is meaningful from the first trade. Creates the ticker if needed.
    pub fn set_previous_close(&mut self, symbol: &str, prev_close: f64, now: DateTime<Utc>) {
        let state = self.tickers.entry(symbol.to_string()).or_insert_with(|| TickerLiveState::new(symbol, now));
        state.previous_close = Some(prev_close);
    }

    /// Initialise per-ticker aggregators and ring buffers on first use.
    fn ensure_ticker_buffers(&mut self, symbol: &str) {
        if self.aggregators.contains_key(symbol) {
            return;
        }
        let mut agg_map = HashMap::new();
        let mut bar_map = HashMap::new();
        for &tf in Timeframe::ALL {
            agg_map.insert(tf, CandleAggregator::new(tf));
            bar_map.insert(tf, RingBuffer::new(RING_CAP));
        }
        self.aggregators.insert(symbol.to_string(), agg_map);
        self.bars.insert(symbol.to_string(), bar_map);
    }

    /// Process one trade tick.
    /// Hot path — only HashMap reads/writes, no allocations beyond the first tick per ticker.
    pub fn on_trade(
        &mut self,
        symbol:       &str,
        price:        f64,
        size:         u64,
        event_time:   DateTime<Utc>,
        now:          DateTime<Utc>,
        warn_ms:      u32,
        critical_ms:  u32,
    ) {
        self.ingest_trade(symbol, price, size, 1, event_time, now, warn_ms, critical_ms);
    }

    /// Market Replay ingestion: identical to `on_trade` but the print may stand
    /// for `prints` real trades (synthetic 10s slices built from minute bars
    /// carry the bar's `n` spread across the slices). `now` = the simulated
    /// instant (latency is meaningless in replay and reads as ~0).
    pub fn on_replay_trade(
        &mut self,
        symbol:     &str,
        price:      f64,
        size:       u64,
        prints:     u64,
        event_time: DateTime<Utc>,
    ) {
        self.ingest_trade(symbol, price, size, prints.max(1), event_time, event_time, 1_000, 2_000);
    }

    #[allow(clippy::too_many_arguments)]
    fn ingest_trade(
        &mut self,
        symbol:       &str,
        price:        f64,
        size:         u64,
        prints:       u64,
        event_time:   DateTime<Utc>,
        now:          DateTime<Utc>,
        warn_ms:      u32,
        critical_ms:  u32,
    ) {
        self.ensure_ticker_buffers(symbol);

        let latency_ms = (now - event_time)
            .num_milliseconds()
            .clamp(0, u32::MAX as i64) as u32;

        // Update VWAP accumulator before borrowing tickers mutably
        let acc = self.vwap_acc.entry(symbol.to_string()).or_insert((0.0, 0.0));
        acc.0 += price * size as f64;
        acc.1 += size as f64;
        let new_vwap = if acc.1 > 0.0 { Some(acc.0 / acc.1) } else { None };

        // Read previous close before the mutable borrow of tickers
        let prev_close = self.tickers.get(symbol).and_then(|s| s.previous_close);
        let change_pct = prev_close.and_then(|pc| {
            if pc > 0.0 { Some((price - pc) / pc * 100.0) } else { None }
        });

        // Update live ticker state
        let state = self.tickers.entry(symbol.to_string()).or_insert_with(|| TickerLiveState::new(symbol, now));

        state.last_price    = Some(price);
        state.volume_day   += size;
        state.high_day      = Some(state.high_day.map_or(price, |h| h.max(price)));
        state.low_day       = Some(state.low_day.map_or(price, |l| l.min(price)));
        state.vwap          = new_vwap;
        state.latency_ui_ms = Some(latency_ms);
        if let Some(cp) = change_pct {
            state.change_day_pct = Some(cp);
        }
        state.updated_at = now;
        // `state` borrow ends here — safe to access other fields below

        // Update global latency
        self.latency = latency::compute(event_time, now, warn_ms, critical_ms);

        // Feed per-timeframe candle aggregators
        let agg_map = self.aggregators.get_mut(symbol).unwrap();
        let bar_map = self.bars.get_mut(symbol).unwrap();
        for (&tf, agg) in agg_map.iter_mut() {
            if let Some(closed) = agg.on_trade_n(price, size, prints, event_time) {
                bar_map.get_mut(&tf).unwrap().push(closed);
            }
        }

        // Record the print(s) in the rolling trade-rate counter (acceleration).
        self.trade_counts
            .entry(symbol.to_string())
            .or_default()
            .record(event_time.timestamp(), prints.min(u32::MAX as u64) as u32);
    }

    /// Wipe every piece of market DATA (tickers, candles, VWAP/trade counters,
    /// news) while keeping the feed/news diagnostics structs. Used by Market
    /// Replay on start, backward seek and stop, so the simulated day never mixes
    /// with live data (and vice-versa).
    pub fn reset_data(&mut self) {
        self.tickers.clear();
        self.aggregators.clear();
        self.bars.clear();
        self.vwap_acc.clear();
        self.trade_counts.clear();
        self.news_by_symbol.clear();
        self.news_log.clear();
    }

    /// Number of trade prints for `symbol` over the last `secs` seconds. None
    /// when the symbol has never printed. Used by micro_pullback's acceleration
    /// trigger via the scanner.
    pub fn trades_in_last(&self, symbol: &str, secs: i64, now: DateTime<Utc>) -> Option<u64> {
        self.trade_counts
            .get(symbol)
            .map(|tc| tc.count_last(now.timestamp(), secs))
    }

    // ── News investor (Alpaca news WebSocket, premarket) ───────────────────────

    /// Ingest one news headline: store it per referenced symbol (correlation) and
    /// in the flat debug log, prune stale entries, and update the diagnostics.
    pub fn on_news(&mut self, news: NewsHeadline) {
        let now = news.received_at;

        // Per-symbol store (for the micro_pullback correlation lookup).
        for sym in &news.symbols {
            let list = self.news_by_symbol.entry(sym.clone()).or_default();
            list.push(news.clone());
            // Prune stale + cap.
            list.retain(|n| (now - n.received_at).num_seconds() < NEWS_RETENTION_SECS);
            if list.len() > NEWS_PER_SYMBOL_CAP {
                let drop = list.len() - NEWS_PER_SYMBOL_CAP;
                list.drain(0..drop);
            }
        }
        self.news_by_symbol.retain(|_, v| !v.is_empty());

        // Flat debug log (newest first).
        self.news_log.push_front(news.clone());
        while self.news_log.len() > NEWS_LOG_CAP {
            self.news_log.pop_back();
        }

        // Diagnostics.
        self.news_feed.news_received += 1;
        self.news_feed.symbols_with_news = self.news_by_symbol.len();
        self.news_feed.last_news_at = Some(now);
        self.news_feed.last_headline = Some(news.headline.clone());
        self.news_feed.last_symbols = news.symbols.clone();
        self.news_feed.state = "streaming".into();
        self.news_feed.updated_at = now;
    }

    /// Headlines for `symbol` whose arrival is within `window_secs` of `now`
    /// (oldest first). The list is per-symbol, so every entry genuinely
    /// references this ticker. The caller (micro_pullback) applies the precise
    /// ±window match against the price-event time; `window_secs` is passed wide
    /// enough (≈2× the correlation window) to also cover a headline that lands
    /// after a recent price event.
    pub fn recent_news(
        &self,
        symbol: &str,
        now: DateTime<Utc>,
        window_secs: i64,
    ) -> Vec<NewsRef> {
        match self.news_by_symbol.get(symbol) {
            Some(list) => list
                .iter()
                .filter(|n| (now - n.received_at).num_seconds().abs() <= window_secs)
                .map(|n| NewsRef { at: n.received_at, headline: n.headline.clone() })
                .collect(),
            None => Vec::new(),
        }
    }

    /// Latest live trade price for `symbol`, or None if unseen. Cheap accessor
    /// (no snapshot clone) for callers that just need the current price.
    pub fn last_price(&self, symbol: &str) -> Option<f64> {
        self.tickers.get(symbol).and_then(|t| t.last_price)
    }

    /// The single most recent live news headline on file for `symbol` (Alpaca
    /// news WebSocket), or None. Used by the enrichment pipeline to surface the
    /// real headline on the chart instead of calling Massive (which carries no
    /// news), matching the source micro_pullback already correlates against.
    pub fn latest_news(&self, symbol: &str) -> Option<NewsHeadline> {
        self.news_by_symbol
            .get(symbol)
            .and_then(|list| list.iter().max_by_key(|n| n.received_at).cloned())
    }

    /// Snapshot of the news diagnostics with the recent log attached, for the
    /// debug panel. (`recent` is filled here rather than kept duplicated in RAM.)
    pub fn news_diagnostics(&self) -> NewsDiagnostics {
        let mut d = self.news_feed.clone();
        d.symbols_with_news = self.news_by_symbol.len();
        d.recent = self.news_log.iter().take(NEWS_LOG_CAP).cloned().collect();
        d
    }

    /// Update the news feed connection state (called by the news stream task).
    pub fn news_state(&mut self, state: &str) {
        self.news_feed.state = state.to_string();
        self.news_feed.updated_at = Utc::now();
    }
    pub fn news_set_premarket(&mut self, in_premarket: bool) {
        self.news_feed.in_premarket = in_premarket;
        self.news_feed.updated_at = Utc::now();
    }
    pub fn news_connected(&mut self) {
        self.news_feed.connected_at = Some(Utc::now());
        self.news_feed.state = "authenticated".into();
        self.news_feed.updated_at = Utc::now();
    }
    pub fn news_error(&mut self, msg: &str) {
        self.news_feed.state = "error".into();
        self.news_feed.last_error = Some(msg.to_string());
        self.news_feed.updated_at = Utc::now();
    }

    /// Process one quote update (bid/ask).
    pub fn on_quote(&mut self, symbol: &str, bid: f64, ask: f64, now: DateTime<Utc>) {
        let state = self.tickers.entry(symbol.to_string()).or_insert_with(|| TickerLiveState::new(symbol, now));
        state.bid    = Some(bid);
        state.ask    = Some(ask);
        state.spread = Some((ask - bid).max(0.0));
        state.updated_at = now;
    }

    /// Clone all live states into a serialisable snapshot for the frontend.
    pub fn snapshot(&self) -> MarketSnapshot {
        MarketSnapshot {
            tickers:      self.tickers.clone(),
            latency:      self.latency.clone(),
            mock_running: self.mock_running,
            live_running: self.live_running,
        }
    }

    /// Get bars for a specific symbol and timeframe (oldest → newest), with the
    /// in-progress (forming) candle appended so the chart updates live instead of
    /// only when a bucket closes.
    pub fn get_bars(&self, symbol: &str, tf: Timeframe) -> Vec<Bar> {
        let raw = self
            .bars
            .get(symbol)
            .and_then(|m| m.get(&tf))
            .map(|rb| rb.as_vec())
            .unwrap_or_default();

        let forming = self
            .aggregators
            .get(symbol)
            .and_then(|m| m.get(&tf))
            .and_then(|a| a.current_bar());

        if tf == Timeframe::Daily {
            // EXACTLY one candle per NY trading day. The ring can transiently hold
            // a day at two slightly different UTC stamps — Alpaca daily bars carry
            // a DST-varying time-of-day (04:00Z in summer / 05:00Z in winter), and
            // a re-stamped forming bar may not match — which a timestamp-keyed
            // dedup would render as two bars for the same day (the premarket "day
            // shown twice" bug). Keying by NY date collapses them to one.
            let mut by_day: std::collections::BTreeMap<NaiveDate, Bar> =
                std::collections::BTreeMap::new();
            for b in raw { by_day.insert(ny_date(b.time), b); }

            if let Some(forming) = forming {
                // The forming daily candle is bucketed to UTC MIDNIGHT (≠ a NY
                // date), so its day is taken from the app clock, not its own stamp.
                // Only fold it when it actually covers today's UTC day (a fresh
                // trade today): during NY trading hours the bucket's UTC date equals
                // today's, while a stale older bucket (no trade yet today) is
                // ignored so it can't paint a bogus current candle.
                let now = crate::time::now();
                if forming.time.date_naive() == now.date_naive() {
                    let fday = ny_date(now);
                    if let Some(existing) = by_day.get_mut(&fday) {
                        // (a) Today's authoritative bar is already here → fold the
                        // live values in so the current candle moves with each trade
                        // (latest close + any new high/low). Alpaca's open + volume
                        // are kept.
                        existing.high  = existing.high.max(forming.high);
                        existing.low   = existing.low.min(forming.low);
                        existing.close = forming.close;
                    } else {
                        // (b) No bar for today yet (premarket / Alpaca lag) → append
                        // the live candle AS today's bar so the chart shows the
                        // current day instead of a gap. Stamp it by shifting the last
                        // bar's timestamp forward by the whole-day gap, so it
                        // inherits the series' own time-of-day (and DST quirk) and
                        // lands on the right NY-day axis label; Alpaca's later
                        // authoritative bar — at the same stamp — replaces it cleanly.
                        let last = by_day.iter().next_back().map(|(d, b)| (*d, b.time));
                        if last.map_or(true, |(d, _)| fday > d) {
                            let mut today = forming;
                            if let Some((last_day, last_time)) = last {
                                today.time = last_time + Duration::days((fday - last_day).num_days());
                            }
                            by_day.insert(fday, today);
                        }
                        // else: a stale forming bar older than the series → ignore.
                    }
                }
            }
            return by_day.into_values().collect();
        }

        // Intraday: dedup by bar open time (multiple bars per day), keeping the
        // series strictly ascending. The ring can transiently hold two bars at the
        // same timestamp — e.g. a partial current-minute bar spliced in by
        // `load_chart_bars`, then the final closed bar pushed by the
        // aggregator/stream — which would otherwise make `setData` reject the series.
        let mut by_time: std::collections::BTreeMap<i64, Bar> = std::collections::BTreeMap::new();
        for b in raw { by_time.insert(b.time.timestamp(), b); }
        let mut bars: Vec<Bar> = by_time.into_values().collect();

        if let Some(forming) = forming {
            match bars.last() {
                Some(last) if last.time == forming.time => *bars.last_mut().unwrap() = forming,
                _ => bars.push(forming),
            }
        }
        bars
    }

    /// Ingest a closed 1-minute bar from Alpaca's `bars` channel (the broad
    /// surveillance tier during the regular session). Updates the live ticker
    /// state and the M1 ring buffer. Ingested for every symbol — including focus
    /// symbols — so a displayed chart keeps moving each minute even when the
    /// symbol is too quiet to print trade ticks; the trades add finer detail.
    #[allow(clippy::too_many_arguments)]
    pub fn on_bar(
        &mut self,
        symbol:    &str,
        bar_time:  DateTime<Utc>,
        open:      f64,
        high:      f64,
        low:       f64,
        close:     f64,
        volume:    u64,
        vw:        f64,
        n:         Option<u64>,
        now:       DateTime<Utc>,
    ) {
        self.ensure_ticker_buffers(symbol);

        let prev_close = self.tickers.get(symbol).and_then(|s| s.previous_close);
        let change_pct = prev_close.and_then(|pc| if pc > 0.0 { Some((close - pc) / pc * 100.0) } else { None });

        // Accumulate session VWAP from the bar's volume-weighted price.
        let acc = self.vwap_acc.entry(symbol.to_string()).or_insert((0.0, 0.0));
        acc.0 += vw * volume as f64;
        acc.1 += volume as f64;
        let new_vwap = if acc.1 > 0.0 { Some(acc.0 / acc.1) } else { None };

        let state = self.tickers.entry(symbol.to_string()).or_insert_with(|| TickerLiveState::new(symbol, now));
        state.last_price  = Some(close);
        state.volume_day += volume;
        state.high_day    = Some(state.high_day.map_or(high, |h| h.max(high)));
        state.low_day     = Some(state.low_day.map_or(low, |l| l.min(low)));
        state.vwap        = new_vwap;
        if let Some(cp) = change_pct {
            state.change_day_pct = Some(cp);
        }
        state.updated_at = now;

        // Push the closed minute bar to the M1 ring buffer (chart history).
        if let Some(rb) = self.bars.get_mut(symbol).and_then(|m| m.get_mut(&Timeframe::M1)) {
            rb.push(Bar { time: bar_time, open, high, low, close, volume, vwap: Some(vw), trade_count: n });
        }
    }

    /// Number of closed bars currently held in RAM for (symbol, tf).
    pub fn bar_count(&self, symbol: &str, tf: Timeframe) -> usize {
        self.bars
            .get(symbol)
            .and_then(|m| m.get(&tf))
            .map(|rb| rb.len())
            .unwrap_or(0)
    }

    /// Splice historical bars (ascending) into the RAM ring buffer for
    /// (symbol, tf), deduped by bar open time. Live bars already in RAM win over
    /// historical ones at the same timestamp, and the result stays ascending so
    /// `get_bars` keeps returning a clean series. Used to backfill a chart/pane
    /// the first time it is shown.
    pub fn merge_history_bars(&mut self, symbol: &str, tf: Timeframe, history: Vec<Bar>) {
        if history.is_empty() {
            return;
        }
        self.ensure_ticker_buffers(symbol);
        let existing = self
            .bars
            .get(symbol)
            .and_then(|m| m.get(&tf))
            .map(|rb| rb.as_vec())
            .unwrap_or_default();

        // Merge. Alpaca's REST history is authoritative for CLOSED bars (it has no
        // gaps and carries the final OHLCV), so it wins over any colliding bar
        // already in RAM — this both fills gaps and refreshes stale partial bars on
        // every chart open. The in-progress forming candle lives in the aggregator
        // (not this ring) and is appended later by `get_bars`, so it is unaffected
        // by the overwrite here. Daily bars are keyed by NY trading day so the same
        // day can never sit in the ring twice (DST-varying stamps); intraday bars
        // are keyed by exact open time (many bars per day).
        let merged: Vec<Bar> = if tf == Timeframe::Daily {
            let mut by_day: std::collections::BTreeMap<NaiveDate, Bar> =
                std::collections::BTreeMap::new();
            for b in existing { by_day.insert(ny_date(b.time), b); }
            for b in history  { by_day.insert(ny_date(b.time), b); }
            by_day.into_values().collect()
        } else {
            let mut by_time: std::collections::BTreeMap<i64, Bar> = std::collections::BTreeMap::new();
            for b in existing { by_time.insert(b.time.timestamp(), b); }
            for b in history  { by_time.insert(b.time.timestamp(), b); }
            by_time.into_values().collect()
        };

        if let Some(rb) = self.bars.get_mut(symbol).and_then(|m| m.get_mut(&tf)) {
            rb.replace_with(merged);
        }
    }

    /// Closed bars only (the ring buffer, oldest → newest, deduped by timestamp)
    /// — excludes the in-progress forming candle that `get_bars` appends. Used by
    /// the Perfect Pullback engine, whose gate logic must run on completed
    /// candles.
    pub fn closed_bars(&self, symbol: &str, tf: Timeframe) -> Vec<Bar> {
        let raw = self
            .bars
            .get(symbol)
            .and_then(|m| m.get(&tf))
            .map(|rb| rb.as_vec())
            .unwrap_or_default();
        let mut by_time: std::collections::BTreeMap<i64, Bar> = std::collections::BTreeMap::new();
        for b in raw { by_time.insert(b.time.timestamp(), b); }
        by_time.into_values().collect()
    }

}

impl Default for MarketState {
    fn default() -> Self {
        Self::new()
    }
}
