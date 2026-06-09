//! Streamable universes. Two definitions only:
//!
//! - **US Stocks** — every active tradable US equity. Streamed during pre-open
//!   and open.
//! - **Low Float** — tradable equities whose float is below `low_float_max`
//!   (no market-cap / price / volume filter). Streamed during premarket to keep
//!   the WebSocket light, since premarket strategies only target low-float names.
//!
//! Membership is resolved by querying SQLite (`universe_repository`), where the
//! startup pipeline stores every tradable asset together with its FMP float.
//! The single live WebSocket switches between these sets when the active
//! session tab changes (see `alpaca::stream`).

/// Default float ceiling (shares) for the Low Float universe.
pub const DEFAULT_LOW_FLOAT_MAX: u64 = 30_000_000;

/// True when an asset belongs to the Low Float universe.
pub fn is_low_float(float_shares: Option<i64>, low_float_max: i64) -> bool {
    matches!(float_shares, Some(f) if f < low_float_max)
}
