// Per-zone trade context stored in RAM only (no persistence in V1).
// Each zone tracks its SL, TP, and tradeID independently.

use std::collections::HashMap;
use chrono::Utc;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZoneTradeContext {
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
    zones: HashMap<String, ZoneTradeContext>,
}

impl ChartState {
    pub fn new() -> Self {
        Self { zones: HashMap::new() }
    }

    fn entry(&mut self, zone_id: &str, symbol: &str, strategy_id: &str) -> &mut ZoneTradeContext {
        self.zones
            .entry(zone_id.to_string())
            .or_insert_with(|| ZoneTradeContext {
                zone_id:     zone_id.to_string(),
                symbol:      symbol.to_string(),
                strategy_id: strategy_id.to_string(),
                trade_id:    None,
                stop_loss:   None,
                take_profit: None,
                closed:      false,
            })
    }

    fn gen_trade_id(symbol: &str, strategy_id: &str) -> String {
        let ts    = Utc::now().format("%y%m%d%H%M%S").to_string();
        let strat = strategy_id.to_uppercase().replace('-', "_");
        format!("{}-{}-{}", ts, symbol.to_uppercase(), strat)
    }

    pub fn get_context(&self, zone_id: &str) -> Option<ZoneTradeContext> {
        self.zones.get(zone_id).cloned()
    }

    /// Returns existing tradeID or generates a new one.
    pub fn create_or_get_trade_id(
        &mut self,
        zone_id:     &str,
        symbol:      &str,
        strategy_id: &str,
    ) -> String {
        let z = self.entry(zone_id, symbol, strategy_id);
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
        let z = self.entry(zone_id, symbol, strategy_id);
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
        let z = self.entry(zone_id, symbol, strategy_id);
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

    /// Called when a zone is released; removes its context.
    pub fn clear_zone(&mut self, zone_id: &str) {
        self.zones.remove(zone_id);
    }

    /// Called when a trade closes (position flat). Clears the owning zone's
    /// SL/TP so the chart bracket lines disappear, but KEEPS the tradeID and
    /// flags the zone as `closed` — the journal/screenshots can still be filled
    /// in for the closed trade until a new SL or TP is placed (which then starts
    /// a fresh trade). The zone keeps its symbol/strategy. Returns the zone_id.
    pub fn reset_closed_trade(&mut self, trade_id: &str) -> Option<String> {
        let zone_id = self
            .zones
            .iter()
            .find(|(_, c)| c.trade_id.as_deref() == Some(trade_id))
            .map(|(z, _)| z.clone())?;
        if let Some(c) = self.zones.get_mut(&zone_id) {
            c.stop_loss   = None;
            c.take_profit = None;
            c.closed      = true;
        }
        Some(zone_id)
    }
}
