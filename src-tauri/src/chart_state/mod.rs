// Trade context (SL / TP / tradeID) stored in RAM, keyed by TICKER (symbol) so it
// belongs to the ticker, not the chart slot: it persists as a zone swaps between
// tickers and follows the ticker (incl. coming back to it later). A lightweight
// zone→symbol map lets the zone_id-based commands (orders, screenshots, close)
// resolve the zone's current ticker without changing their signatures.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

/// Monotonic within the process — the tie-breaker in every generated tradeID.
static TRADE_SEQ: AtomicU64 = AtomicU64::new(0);

/// Short, sortable-enough base-36 rendering of the counter (0, 1, … 9, a, b, …).
fn base36(mut n: u64) -> String {
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if n == 0 {
        return "0".into();
    }
    let mut out = Vec::new();
    while n > 0 {
        out.push(DIGITS[(n % 36) as usize]);
        n /= 36;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZoneTradeContext {
    /// Kept for the frontend type; now mirrors `symbol` (context is per-ticker).
    pub zone_id: String,
    pub symbol: String,
    pub strategy_id: String,
    pub trade_id: Option<String>,
    pub stop_loss: Option<f64>,
    pub take_profit: Option<f64>,
    /// True once the trade has closed (position flat). The tradeID is kept so
    /// the journal/screenshots can still be filled in, but the next SL/TP placed
    /// on the chart starts a brand-new trade (new tradeID).
    pub closed: bool,
}

/// The hotkey engines that take a trade of their own on a ticker: Apex and Dip.
/// Both file it in TradeTally under their own setup name and size it off their own
/// context, so a trade started by hand afterwards must not inherit either.
fn is_engine_strategy(id: &str) -> bool {
    id == crate::apex::STRATEGY_ID || id == crate::dip::STRATEGY_ID
}

/// A new trade started by hand on a ticker whose last trade was an Apex (or Dip) one
/// belongs to the zone's strategy again — otherwise it would be filed in TradeTally
/// as "Apex" / "Dip" and sized off that engine's context.
fn release_engine(z: &mut ZoneTradeContext, caller_strategy_id: &str) {
    if is_engine_strategy(&z.strategy_id)
        && !caller_strategy_id.is_empty()
        && !is_engine_strategy(caller_strategy_id)
    {
        z.strategy_id = caller_strategy_id.to_string();
    }
}

#[derive(Clone)]
pub struct ChartState {
    /// Trade context per ticker (symbol).
    contexts: HashMap<String, ZoneTradeContext>,
    /// Which ticker each zone currently shows, so zone_id-based commands resolve
    /// the symbol. Updated whenever the frontend loads or mutates a zone.
    zone_symbol: HashMap<String, String>,
}

impl ChartState {
    pub fn new() -> Self {
        Self {
            contexts: HashMap::new(),
            zone_symbol: HashMap::new(),
        }
    }

    fn entry(&mut self, symbol: &str, strategy_id: &str) -> &mut ZoneTradeContext {
        self.contexts
            .entry(symbol.to_string())
            .or_insert_with(|| ZoneTradeContext {
                zone_id: symbol.to_string(),
                symbol: symbol.to_string(),
                strategy_id: strategy_id.to_string(),
                trade_id: None,
                stop_loss: None,
                take_profit: None,
                closed: false,
            })
    }

    /// `<ts>-<SYMBOL>-<STRATEGY>-<seq>`.
    ///
    /// The trailing counter is not decoration. The timestamp has second resolution
    /// and, in a Market Replay, comes from a clock that moves in jumps — so closing
    /// a trade and planning the next one on the same ticker inside one second used
    /// to mint the very SAME id twice. TradeTally already had a mapping on file for
    /// it, skipped the create, and filed the new trade's fills onto the old trade.
    /// The counter makes a collision impossible for as long as the process lives,
    /// which is the whole window in which two ids can share a second.
    ///
    /// Nothing anywhere parses the shape of an id — it is an opaque string to the
    /// queue, the marker in TradeTally's notes and the screenshot filenames alike.
    fn gen_trade_id(symbol: &str, strategy_id: &str) -> String {
        let ts = crate::time::now().format("%y%m%d%H%M%S").to_string();
        let strat = strategy_id.to_uppercase().replace('-', "_");
        let seq = base36(TRADE_SEQ.fetch_add(1, Ordering::Relaxed));
        format!("{}-{}-{}-{}", ts, symbol.to_uppercase(), strat, seq)
    }

    /// Record which ticker a zone currently shows (so zone_id lookups resolve it).
    fn track_zone(&mut self, zone_id: &str, symbol: &str) {
        self.zone_symbol
            .insert(zone_id.to_string(), symbol.to_string());
    }

    /// Context for a ticker (by symbol).
    pub fn get_context(&self, symbol: &str) -> Option<ZoneTradeContext> {
        self.contexts.get(symbol).cloned()
    }

    /// Snapshot of all per-ticker trade contexts, for persistence. The
    /// `zone_symbol` map is intentionally excluded: it's per-session layout state
    /// the frontend re-establishes via `load_zone_context` on load.
    pub fn export_contexts(&self) -> Vec<ZoneTradeContext> {
        self.contexts.values().cloned().collect()
    }

    /// Restore per-ticker trade contexts from a persisted snapshot (called once at
    /// startup). Keyed by symbol so SL/TP/tradeID lines reappear on the chart for
    /// an open multi-day trade after a restart.
    pub fn import_contexts(&mut self, ctxs: Vec<ZoneTradeContext>) {
        for c in ctxs {
            self.contexts.insert(c.symbol.clone(), c);
        }
    }

    /// Replace all trading contexts while preserving the live zone→symbol layout.
    /// Market Replay uses this to isolate simulated SL/TP/trade ids, then restore the
    /// live contexts on exit without forcing the frontend to rebuild its zones.
    pub fn replace_contexts(&mut self, ctxs: Vec<ZoneTradeContext>) {
        self.contexts.clear();
        self.import_contexts(ctxs);
    }

    /// The ticker a zone currently shows, whether or not it has a trade context.
    /// `get_context_for_zone` answers None for a ticker that was never traded, which
    /// is exactly the case the release path still needs an answer for.
    pub fn symbol_for_zone(&self, zone_id: &str) -> Option<String> {
        self.zone_symbol.get(zone_id).cloned()
    }

    /// Context of the ticker a zone currently shows (zone_id → symbol → context).
    /// Used by the in-zone order / screenshot / close commands, which only carry a
    /// zone_id. Returns None when the zone holds no (known) ticker yet.
    pub fn get_context_for_zone(&self, zone_id: &str) -> Option<ZoneTradeContext> {
        let symbol = self.zone_symbol.get(zone_id)?;
        self.contexts.get(symbol).cloned()
    }

    /// Frontend load path: record the zone's current ticker, then return that
    /// ticker's context. The context follows the ticker, so swapping a zone's
    /// ticker shows the new ticker's own SL/TP (and coming back restores it).
    pub fn load_zone_context(&mut self, zone_id: &str, symbol: &str) -> Option<ZoneTradeContext> {
        self.track_zone(zone_id, symbol);
        self.get_context(symbol)
    }

    /// Returns existing tradeID or generates a new one (per ticker).
    pub fn create_or_get_trade_id(
        &mut self,
        zone_id: &str,
        symbol: &str,
        strategy_id: &str,
    ) -> String {
        self.track_zone(zone_id, symbol);
        let z = self.entry(symbol, strategy_id);
        if z.trade_id.is_none() {
            z.trade_id = Some(Self::gen_trade_id(symbol, strategy_id));
        }
        z.trade_id.clone().unwrap()
    }

    /// Sets SL; auto-generates tradeID on first non-null price.
    pub fn update_sl(
        &mut self,
        zone_id: &str,
        symbol: &str,
        strategy_id: &str,
        price: Option<f64>,
    ) -> ZoneTradeContext {
        self.track_zone(zone_id, symbol);
        let z = self.entry(symbol, strategy_id);
        // A previously-closed trade: placing a fresh SL starts a NEW trade.
        if price.is_some() && z.closed {
            z.trade_id = None;
            z.closed = false;
        }
        if price.is_some() && z.trade_id.is_none() {
            release_engine(z, strategy_id);
        }
        z.stop_loss = price;
        if price.is_some() && z.trade_id.is_none() {
            z.trade_id = Some(Self::gen_trade_id(symbol, strategy_id));
        }
        z.clone()
    }

    /// Sets TP; auto-generates tradeID on first non-null price.
    pub fn update_tp(
        &mut self,
        zone_id: &str,
        symbol: &str,
        strategy_id: &str,
        price: Option<f64>,
    ) -> ZoneTradeContext {
        self.track_zone(zone_id, symbol);
        let z = self.entry(symbol, strategy_id);
        // A previously-closed trade: placing a fresh TP starts a NEW trade.
        if price.is_some() && z.closed {
            z.trade_id = None;
            z.closed = false;
        }
        if price.is_some() && z.trade_id.is_none() {
            release_engine(z, strategy_id);
        }
        z.take_profit = price;
        if price.is_some() && z.trade_id.is_none() {
            z.trade_id = Some(Self::gen_trade_id(symbol, strategy_id));
        }
        z.clone()
    }

    /// Rub out a tradeID that never became a trade.
    ///
    /// Placing an SL or a TP mints a tradeID on the spot — that is what lets a trade
    /// be journalled and screenshotted before it is entered. Taking the same level
    /// straight back off used to leave the id standing, and the ticker's context then
    /// pointed at a trade that had never existed: nothing to attach a note or a chart
    /// to upstream, so the note was filed against a trade TradeTally could not find
    /// and the previous, REAL trade on the ticker was no longer reachable from the
    /// chart. Hence: no level left, and no execution against the id → the id goes
    /// with the level.
    ///
    /// `had_activity` is the caller's answer to "did this trade ever fill?"; this
    /// type has no way to know. Returns the context as it now stands.
    pub fn discard_planned_trade(
        &mut self,
        symbol: &str,
        had_activity: bool,
    ) -> Option<ZoneTradeContext> {
        let z = self.contexts.get_mut(symbol)?;
        if had_activity
            || z.trade_id.is_none()
            || z.stop_loss.is_some()
            || z.take_profit.is_some()
        {
            return Some(z.clone());
        }
        z.trade_id = None;
        // Never traded, so there is nothing to have closed.
        z.closed = false;
        Some(z.clone())
    }

    /// Turn the ticker's context into a fresh trade owned by a hotkey engine: its own
    /// tradeID, that engine's strategy id (TradeTally setup "Apex" / "Dip") and its
    /// bracket. Whatever planned levels were drawn are replaced — the engine's
    /// bracket is the trade's definition.
    /// The caller guarantees no position is open on the ticker.
    pub fn start_engine_trade(
        &mut self,
        zone_id: &str,
        symbol: &str,
        strategy_id: &str,
        stop_loss: f64,
        take_profit: Option<f64>,
    ) -> ZoneTradeContext {
        self.track_zone(zone_id, symbol);
        let ctx = ZoneTradeContext {
            zone_id: symbol.to_string(),
            symbol: symbol.to_string(),
            strategy_id: strategy_id.to_string(),
            trade_id: Some(Self::gen_trade_id(symbol, strategy_id)),
            stop_loss: Some(stop_loss),
            // None under Apex's dynamic take profit: no target line, no bracket leg.
            take_profit,
            closed: false,
        };
        self.contexts.insert(symbol.to_string(), ctx.clone());
        ctx
    }

    /// `start_engine_trade` for Apex, the engine that had it first.
    pub fn start_apex_trade(
        &mut self,
        zone_id: &str,
        symbol: &str,
        stop_loss: f64,
        take_profit: Option<f64>,
    ) -> ZoneTradeContext {
        self.start_engine_trade(
            zone_id,
            symbol,
            crate::apex::STRATEGY_ID,
            stop_loss,
            take_profit,
        )
    }

    /// Put a ticker's context back exactly as it was (an Apex order the venue
    /// refused must not leave its tradeID and lines behind).
    pub fn restore_context(&mut self, symbol: &str, previous: Option<ZoneTradeContext>) {
        match previous {
            Some(c) => {
                self.contexts.insert(symbol.to_string(), c);
            }
            None => {
                self.contexts.remove(symbol);
            }
        }
    }

    /// Called when a zone is released: forget which ticker it showed. The ticker's
    /// trade context is KEPT (it belongs to the ticker, not the zone).
    pub fn clear_zone(&mut self, zone_id: &str) {
        self.zone_symbol.remove(zone_id);
    }

    /// Mirrors a backend-driven SL/TP change (auto breakeven / auto take-profit,
    /// from the trading loop) into the ticker's chart context, so the on-chart
    /// line matches the position it just adjusted. Unlike `update_sl`/`update_tp`
    /// this never creates a context or a tradeID — the open position that
    /// triggered the change already has both. No-op if the ticker has no context
    /// (shouldn't happen: an open position always has one).
    pub fn sync_levels(&mut self, symbol: &str, stop_loss: Option<f64>, take_profit: Option<f64>) {
        if let Some(c) = self.contexts.get_mut(symbol) {
            c.stop_loss = stop_loss;
            c.take_profit = take_profit;
        }
    }

    /// Called when a trade closes (position flat). Clears the owning TICKER's
    /// SL/TP so the chart bracket lines disappear, but KEEPS the tradeID and flags
    /// the context `closed` — the journal/screenshots can still be filled in for
    /// the closed trade until a new SL or TP is placed (which starts a fresh
    /// trade). Returns the symbol whose trade was reset.
    pub fn reset_closed_trade(&mut self, trade_id: &str) -> Option<String> {
        let symbol = self
            .contexts
            .iter()
            .find(|(_, c)| c.trade_id.as_deref() == Some(trade_id))
            .map(|(s, _)| s.clone())?;
        if let Some(c) = self.contexts.get_mut(&symbol) {
            c.stop_loss = None;
            c.take_profit = None;
            c.closed = true;
        }
        Some(symbol)
    }
}

#[cfg(test)]
mod trade_link_tests {
    //! The tradeID a ticker carries between trades. What matters here is that a
    //! LINE drawn on the chart never leaves a trade behind it once it is rubbed out:
    //! the journal and the screenshot bind to whatever id the context holds, and an
    //! id that stands for nothing sends them to a trade TradeTally cannot find.

    use super::*;

    /// The exact sequence that used to lose the link: close a trade, drop an SL,
    /// take it straight back off.
    #[test]
    fn a_level_placed_and_removed_leaves_no_trade_behind() {
        let mut cs = ChartState::new();
        // A real trade, closed.
        let real = cs
            .update_sl("z1", "ABCD", "micro_pullback", Some(4.0))
            .trade_id
            .unwrap();
        cs.reset_closed_trade(&real);

        // A fresh SL mints a new id — that is what lets a trade be journalled before
        // it is entered, so it is expected.
        let planned = cs
            .update_sl("z1", "ABCD", "micro_pullback", Some(3.9))
            .trade_id
            .unwrap();
        assert_ne!(planned, real);

        // Taking it straight back off: it never traded, so it leaves nothing behind.
        cs.update_sl("z1", "ABCD", "micro_pullback", None);
        let ctx = cs
            .discard_planned_trade("ABCD", false)
            .expect("the ticker still has a context");
        assert_eq!(ctx.trade_id, None, "the phantom id is gone");
        assert!(!ctx.closed);
    }

    #[test]
    fn a_trade_that_executed_keeps_its_id_when_its_levels_are_cleared() {
        let mut cs = ChartState::new();
        let id = cs
            .update_sl("z1", "ABCD", "micro_pullback", Some(4.0))
            .trade_id
            .unwrap();
        cs.update_sl("z1", "ABCD", "micro_pullback", None);
        let ctx = cs.discard_planned_trade("ABCD", true).unwrap();
        assert_eq!(
            ctx.trade_id.as_deref(),
            Some(id.as_str()),
            "a filled trade survives its lines being cleared"
        );
    }

    #[test]
    fn a_level_still_on_the_chart_holds_its_planned_trade() {
        let mut cs = ChartState::new();
        let id = cs
            .update_sl("z1", "ABCD", "micro_pullback", Some(4.0))
            .trade_id
            .unwrap();
        // Only the TP is cleared; the SL is still drawn, so the plan stands.
        cs.update_tp("z1", "ABCD", "micro_pullback", None);
        let ctx = cs.discard_planned_trade("ABCD", false).unwrap();
        assert_eq!(ctx.trade_id.as_deref(), Some(id.as_str()));
    }

    /// Two trades minted on the same ticker inside one second are two trades.
    #[test]
    fn two_trades_in_the_same_second_never_share_an_id() {
        let mut cs = ChartState::new();
        let first = cs
            .update_sl("z1", "ABCD", "micro_pullback", Some(4.0))
            .trade_id
            .unwrap();
        cs.reset_closed_trade(&first);
        let second = cs
            .update_sl("z1", "ABCD", "micro_pullback", Some(3.9))
            .trade_id
            .unwrap();
        assert_ne!(first, second);
    }

    #[test]
    fn a_ticker_that_was_never_touched_has_nothing_to_discard() {
        let mut cs = ChartState::new();
        assert!(cs.discard_planned_trade("ABCD", false).is_none());
    }

    #[test]
    fn a_released_zone_no_longer_resolves_to_its_ticker() {
        let mut cs = ChartState::new();
        cs.load_zone_context("z1", "ABCD");
        assert_eq!(cs.symbol_for_zone("z1").as_deref(), Some("ABCD"));
        cs.clear_zone("z1");
        assert_eq!(cs.symbol_for_zone("z1"), None);
    }

    /// The release path needs the ticker even when it has no trade context at all —
    /// that is the common case, a chart opened and released without a trade.
    #[test]
    fn the_zone_ticker_is_known_before_any_trade_exists() {
        let mut cs = ChartState::new();
        cs.load_zone_context("z1", "ABCD");
        assert!(cs.get_context_for_zone("z1").is_none());
        assert_eq!(cs.symbol_for_zone("z1").as_deref(), Some("ABCD"));
    }
}

#[cfg(test)]
mod apex_tests {
    use super::*;

    #[test]
    fn an_apex_trade_gets_its_own_trade_id_and_strategy() {
        let mut cs = ChartState::new();
        cs.update_sl("z1", "ABCD", "micro_pullback", Some(4.0));
        let ctx = cs.start_apex_trade("z1", "ABCD", 4.10, Some(4.30));
        assert_eq!(ctx.strategy_id, "apex");
        assert!(ctx.trade_id.as_deref().unwrap().contains("-ABCD-APEX-"));
        assert_eq!((ctx.stop_loss, ctx.take_profit), (Some(4.10), Some(4.30)));
        assert_eq!(cs.get_context_for_zone("z1").unwrap().strategy_id, "apex");
    }

    #[test]
    fn a_dynamic_take_profit_leaves_no_tp_line() {
        let mut cs = ChartState::new();
        let ctx = cs.start_apex_trade("z1", "ABCD", 4.10, None);
        assert_eq!(ctx.take_profit, None);
        assert_eq!(ctx.stop_loss, Some(4.10));
    }

    #[test]
    fn moving_an_open_apex_stop_keeps_it_an_apex_trade() {
        let mut cs = ChartState::new();
        let id = cs.start_apex_trade("z1", "ABCD", 4.10, Some(4.30)).trade_id;
        let ctx = cs.update_sl("z1", "ABCD", "micro_pullback", Some(4.15));
        assert_eq!((ctx.strategy_id.as_str(), ctx.trade_id), ("apex", id));
    }

    #[test]
    fn the_next_manual_trade_after_apex_belongs_to_the_zone_strategy() {
        let mut cs = ChartState::new();
        let id = cs
            .start_apex_trade("z1", "ABCD", 4.10, Some(4.30))
            .trade_id
            .unwrap();
        cs.reset_closed_trade(&id);
        let ctx = cs.update_sl("z1", "ABCD", "micro_pullback", Some(4.0));
        assert_eq!(ctx.strategy_id, "micro_pullback");
        assert_ne!(ctx.trade_id.as_deref(), Some(id.as_str()));
    }

    #[test]
    fn a_refused_apex_order_restores_the_previous_context() {
        let mut cs = ChartState::new();
        let before = cs.update_tp("z1", "ABCD", "micro_pullback", Some(5.0));
        let prev = cs.get_context("ABCD");
        cs.start_apex_trade("z1", "ABCD", 4.10, Some(4.30));
        cs.restore_context("ABCD", prev);
        let after = cs.get_context("ABCD").unwrap();
        assert_eq!(after.trade_id, before.trade_id);
        assert_eq!(after.strategy_id, "micro_pullback");

        let mut fresh = ChartState::new();
        fresh.start_apex_trade("z1", "WXYZ", 1.0, Some(1.2));
        fresh.restore_context("WXYZ", None);
        assert!(fresh.get_context("WXYZ").is_none());
    }
}
