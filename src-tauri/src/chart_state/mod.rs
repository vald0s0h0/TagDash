// Trade context (SL / TP / tradeID) stored in RAM, keyed by TICKER (symbol) so it
// belongs to the ticker, not the chart slot: it persists as a zone swaps between
// tickers and follows the ticker (incl. coming back to it later). A lightweight
// zone→symbol map lets the zone_id-based commands (orders, screenshots, close)
// resolve the zone's current ticker without changing their signatures.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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

    fn gen_trade_id(symbol: &str, strategy_id: &str) -> String {
        let ts = crate::time::now().format("%y%m%d%H%M%S").to_string();
        let strat = strategy_id.to_uppercase().replace('-', "_");
        format!("{}-{}-{}", ts, symbol.to_uppercase(), strat)
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
mod apex_tests {
    use super::*;

    #[test]
    fn an_apex_trade_gets_its_own_trade_id_and_strategy() {
        let mut cs = ChartState::new();
        cs.update_sl("z1", "ABCD", "micro_pullback", Some(4.0));
        let ctx = cs.start_apex_trade("z1", "ABCD", 4.10, Some(4.30));
        assert_eq!(ctx.strategy_id, "apex");
        assert!(ctx.trade_id.as_deref().unwrap().ends_with("-ABCD-APEX"));
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
