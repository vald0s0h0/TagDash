// Trade context (SL / TP / tradeID) stored in RAM, keyed by TICKER (symbol) so it
// belongs to the ticker, not the chart slot: it persists as a zone swaps between
// tickers and follows the ticker (incl. coming back to it later). A lightweight
// zone→symbol map lets the zone_id-based commands (orders, screenshots, close)
// resolve the zone's current ticker without changing their signatures.

use std::collections::HashMap;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZoneTradeContext {
    /// Kept for the frontend type; now mirrors `symbol` (context is per-ticker).
    pub zone_id:     String,
    pub symbol:      String,
    pub strategy_id: String,
    pub trade_id:    Option<String>,
    pub stop_loss:   Option<f64>,
    pub take_profit: Option<f64>,
    /// True once the trade has closed (position flat). The tradeID is kept so
    /// the journal/screenshots can still be filled in, but the next SL/TP placed
    /// on the chart starts a brand-new trade (new tradeID).
    pub closed:      bool,
}

pub struct ChartState {
    /// Trade context per ticker (symbol).
    contexts: HashMap<String, ZoneTradeContext>,
    /// Which ticker each zone currently shows, so zone_id-based commands resolve
    /// the symbol. Updated whenever the frontend loads or mutates a zone.
    zone_symbol: HashMap<String, String>,
}

impl ChartState {
    pub fn new() -> Self {
        Self { contexts: HashMap::new(), zone_symbol: HashMap::new() }
    }

    fn entry(&mut self, symbol: &str, strategy_id: &str) -> &mut ZoneTradeContext {
        self.contexts
            .entry(symbol.to_string())
            .or_insert_with(|| ZoneTradeContext {
                zone_id:     symbol.to_string(),
                symbol:      symbol.to_string(),
                strategy_id: strategy_id.to_string(),
                trade_id:    None,
                stop_loss:   None,
                take_profit: None,
                closed:      false,
            })
    }

    fn gen_trade_id(symbol: &str, strategy_id: &str) -> String {
        let ts    = crate::time::now().format("%y%m%d%H%M%S").to_string();
        let strat = strategy_id.to_uppercase().replace('-', "_");
        format!("{}-{}-{}", ts, symbol.to_uppercase(), strat)
    }

    /// Record which ticker a zone currently shows (so zone_id lookups resolve it).
    fn track_zone(&mut self, zone_id: &str, symbol: &str) {
        self.zone_symbol.insert(zone_id.to_string(), symbol.to_string());
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
        zone_id:     &str,
        symbol:      &str,
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
        zone_id:     &str,
        symbol:      &str,
        strategy_id: &str,
        price:       Option<f64>,
    ) -> ZoneTradeContext {
        self.track_zone(zone_id, symbol);
        let z = self.entry(symbol, strategy_id);
        // A previously-closed trade: placing a fresh SL starts a NEW trade.
        if price.is_some() && z.closed {
            z.trade_id = None;
            z.closed   = false;
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
        zone_id:     &str,
        symbol:      &str,
        strategy_id: &str,
        price:       Option<f64>,
    ) -> ZoneTradeContext {
        self.track_zone(zone_id, symbol);
        let z = self.entry(symbol, strategy_id);
        // A previously-closed trade: placing a fresh TP starts a NEW trade.
        if price.is_some() && z.closed {
            z.trade_id = None;
            z.closed   = false;
        }
        z.take_profit = price;
        if price.is_some() && z.trade_id.is_none() {
            z.trade_id = Some(Self::gen_trade_id(symbol, strategy_id));
        }
        z.clone()
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
            c.stop_loss   = stop_loss;
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
            c.stop_loss   = None;
            c.take_profit = None;
            c.closed      = true;
        }
        Some(symbol)
    }
}
