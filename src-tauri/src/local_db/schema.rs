// SQLite schema. All statements use IF NOT EXISTS so this is safe to run on
// every startup — it is idempotent and never drops existing data.

use rusqlite::{Connection, Result};

const MIGRATIONS: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;

-- Key-value bag for persisted UI/app settings (overflow for tagdash.toml).
CREATE TABLE IF NOT EXISTS app_config (
    key        TEXT PRIMARY KEY NOT NULL,
    value      TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Filtered asset list built at startup from Alpaca + FMP.
CREATE TABLE IF NOT EXISTS universe_assets (
    symbol     TEXT PRIMARY KEY NOT NULL,
    name       TEXT,
    exchange   TEXT,
    tradable   INTEGER NOT NULL DEFAULT 1,
    shortable  INTEGER NOT NULL DEFAULT 0,
    float_shares INTEGER,
    market_cap   INTEGER,
    avg_volume   INTEGER,
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- FMP bulk fundamentals cache (float, outstanding, ATR, multi-day change).
-- change_Nd_pct = close-to-close % change over N trading days (yesterday's close
-- vs the close N bars earlier), recomputed daily from daily_cache. Close-to-close
-- so overnight gaps are included.
CREATE TABLE IF NOT EXISTS fundamentals_cache (
    symbol           TEXT PRIMARY KEY NOT NULL,
    float_shares     INTEGER,
    outstanding_shares INTEGER,
    free_float       REAL,
    prev_close       REAL,
    avg_volume       INTEGER,
    atr              REAL,
    change_1d_pct    REAL,
    change_2d_pct    REAL,
    change_3d_pct    REAL,
    change_4d_pct    REAL,
    change_5d_pct    REAL,
    change_6d_pct    REAL,
    updated_at       TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Company metadata from sec-api.io: country of origin (business HQ, not the
-- listing venue) + SIC industry classification. Refreshed at most once a day.
CREATE TABLE IF NOT EXISTS company_meta (
    symbol     TEXT PRIMARY KEY NOT NULL,
    country    TEXT,
    sic        TEXT,
    industry   TEXT,
    sector     TEXT,
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Daily OHLCV bars per symbol (used to seed strategies at startup).
CREATE TABLE IF NOT EXISTS daily_cache (
    symbol     TEXT NOT NULL,
    date       TEXT NOT NULL,
    open       REAL,
    high       REAL,
    low        REAL,
    close      REAL,
    volume     INTEGER,
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (symbol, date)
);

-- Tags retrieved from TradeTally API at startup.
CREATE TABLE IF NOT EXISTS tradetally_tags (
    tag        TEXT PRIMARY KEY NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Outbound event queue for TradeTally. Drained by a background worker.
-- Status: 'pending' | 'success' | 'failed'
CREATE TABLE IF NOT EXISTS tradetally_sync_queue (
    event_id       TEXT PRIMARY KEY NOT NULL,
    timestamp      TEXT NOT NULL,
    trade_id       TEXT NOT NULL,
    symbol         TEXT NOT NULL,
    event_type     TEXT NOT NULL,
    endpoint       TEXT NOT NULL,
    payload        TEXT NOT NULL,
    status         TEXT NOT NULL DEFAULT 'pending',
    error_message  TEXT,
    attempts       INTEGER NOT NULL DEFAULT 0,
    created_at     TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_sync_queue_status ON tradetally_sync_queue(status);
CREATE INDEX IF NOT EXISTS idx_sync_queue_created ON tradetally_sync_queue(created_at DESC);

-- Scanner alert history (recent N alerts for replay / audit).
CREATE TABLE IF NOT EXISTS alert_history (
    alert_id    TEXT PRIMARY KEY NOT NULL,
    timestamp   TEXT NOT NULL,
    symbol      TEXT NOT NULL,
    strategy_id TEXT NOT NULL,
    priority    INTEGER NOT NULL,
    session     TEXT NOT NULL,
    reason      TEXT NOT NULL,
    created_at  TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_alert_history_ts ON alert_history(created_at DESC);

-- Local paths of captured screenshots.
CREATE TABLE IF NOT EXISTS screenshot_files (
    id         TEXT PRIMARY KEY NOT NULL,
    trade_id   TEXT,
    path       TEXT NOT NULL,
    uploaded   INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Application log entries (info / warn / error).
CREATE TABLE IF NOT EXISTS local_logs (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    level      TEXT NOT NULL,
    message    TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_local_logs_created ON local_logs(created_at DESC);

-- Per-trade journal entries (notes, confidence, tags). Upserted when saved.
CREATE TABLE IF NOT EXISTS journal_entries (
    trade_id    TEXT PRIMARY KEY NOT NULL,
    symbol      TEXT NOT NULL DEFAULT '',
    notes       TEXT NOT NULL DEFAULT '',
    confidence  INTEGER,
    tags_json   TEXT NOT NULL DEFAULT '[]',
    created_at  TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at  TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Mapping our local tradeID to TradeTally's server UUID.
-- Populated after the trade is successfully created in TradeTally.
CREATE TABLE IF NOT EXISTS tradetally_trade_ids (
    local_trade_id  TEXT PRIMARY KEY NOT NULL,
    tt_trade_id     TEXT NOT NULL,
    created_at      TEXT NOT NULL DEFAULT (datetime('now'))
);

-- User-submitted bug reports. Persist across launches.
-- priority: 1 = low, 2 = medium, 3 = high.
CREATE TABLE IF NOT EXISTS bug_reports (
    id         TEXT PRIMARY KEY NOT NULL,
    text       TEXT NOT NULL,
    priority   INTEGER NOT NULL DEFAULT 2,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_bug_reports_sort ON bug_reports(priority DESC, created_at DESC);

-- Price alarms placed on charts. Each alarm is tied to a ticker (and the
-- strategy of the zone/chart it was drawn from). Triggering is not wired yet —
-- this just persists the level so it can be armed later.
CREATE TABLE IF NOT EXISTS price_alarms (
    id           TEXT PRIMARY KEY NOT NULL,
    symbol       TEXT NOT NULL,
    strategy_id  TEXT,
    price        REAL NOT NULL,
    created_at   TEXT NOT NULL DEFAULT (datetime('now')),
    -- Set the moment the price crosses the alarm level (fires an Open alert).
    -- NULL = armed/waiting. Watching is done in the scanner loop.
    triggered_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_price_alarms_symbol ON price_alarms(symbol);

-- Daily mean-reversion scores per ticker (Panic Mean Reversion pre-open screener).
-- Recomputed once per calendar day from daily_cache (see `scoring` module). The
-- display_score is a CONTINUOUS composite of the Bollinger event score, a parabolic
-- (true-range expansion × direction) score, a log-scaled dollar-volume score and a
-- consecutive-candle run score — not a saturating percentile. The cross-sectional
-- percent rank (pr_score) is kept as a diagnostic only. Top 30 by display_score
-- feed the pre-open watchlist.
CREATE TABLE IF NOT EXISTS mean_reversion_scores (
    symbol          TEXT PRIMARY KEY NOT NULL,
    -- Cross-sectional percent rank: DIAGNOSTIC ONLY (no longer part of display).
    pr_score        REAL NOT NULL DEFAULT 0,
    pr_best_days    INTEGER NOT NULL DEFAULT 0,
    bb_event_score  REAL NOT NULL DEFAULT 0,
    bb_best_horizon INTEGER NOT NULL DEFAULT 0,
    -- Continuous composite components (0..1) feeding display_score.
    parabolic_score REAL NOT NULL DEFAULT 0,
    volume_score    REAL NOT NULL DEFAULT 0,
    run_score       REAL NOT NULL DEFAULT 0,
    run_len         INTEGER NOT NULL DEFAULT 0,
    run_dir         INTEGER NOT NULL DEFAULT 0,
    display_score   REAL NOT NULL DEFAULT 0,
    score_kind      TEXT NOT NULL DEFAULT 'MR',
    -- Previous trading day's volume (shares). Used to gate (>20M) and to tie-break
    -- equal scores; also shown as the screener card's volume.
    prev_volume     INTEGER,
    updated_at      TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_mr_scores_display ON mean_reversion_scores(display_score DESC);

-- Persisted LLM analysis RESULTS per ticker (panic mean-reversion button read).
-- Only the model's outputs are stored (context summary + reversion verdict), not
-- the prompt or the fetched news/OHLC. Every call is appended (history); the UI
-- hydrates the most recent row for a symbol when a zone re-opens.
CREATE TABLE IF NOT EXISTS llm_analysis (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    symbol      TEXT NOT NULL,
    strategy_id TEXT NOT NULL,
    context     TEXT,
    verdict     TEXT,
    created_at  TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_llm_analysis_symbol ON llm_analysis(symbol, created_at DESC);

-- Trade executions (internal simulated fills), persisted per ticker so the chart
-- can draw the entry/scale/exit triangles even days later / across restarts.
-- `quantity` is the SIGNED share delta of the fill (+ buy/long, − sell/short),
-- so the running position and realized P&L are reconstructable from the rows.
CREATE TABLE IF NOT EXISTS executions (
    fill_id    TEXT PRIMARY KEY NOT NULL,
    trade_id   TEXT NOT NULL,
    symbol     TEXT NOT NULL,
    quantity   INTEGER NOT NULL,
    fill_price REAL NOT NULL,
    filled_at  TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_executions_symbol ON executions(symbol);

-- Original (launch-time) stop loss per trade. Recorded ONCE on the first fill and
-- never overwritten, so it survives later SL modifications — it's the level the
-- journal/R:R is based on. Drives the thin "original SL" segment drawn on every
-- chart of the symbol for the duration of the trade.
CREATE TABLE IF NOT EXISTS trade_levels (
    trade_id    TEXT PRIMARY KEY NOT NULL,
    symbol      TEXT NOT NULL,
    original_sl REAL,
    created_at  TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_trade_levels_symbol ON trade_levels(symbol);

-- User chart drawings (trend lines + text annotations), persisted PER TICKER so
-- they reappear on every chart/zone showing that symbol and survive restarts.
-- `kind`='line' uses both points (t1,p1)-(t2,p2); 'text' uses (t1,p1)+text.
-- Times are chart Unix-seconds, prices are raw. Pixel fallbacks aren't stored —
-- positions are recomputed from time/price on render.
CREATE TABLE IF NOT EXISTS chart_drawings (
    id         TEXT PRIMARY KEY NOT NULL,
    symbol     TEXT NOT NULL,
    kind       TEXT NOT NULL,
    t1         REAL NOT NULL,
    p1         REAL NOT NULL,
    t2         REAL,
    p2         REAL,
    text       TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_chart_drawings_symbol ON chart_drawings(symbol);

-- Pre-open screener cards the user dismissed. Scoped to a trading DAY (ET date)
-- so a dismissal persists across restarts for the rest of the day, then the
-- ticker can reappear the next day. Old days are pruned on read.
CREATE TABLE IF NOT EXISTS screener_dismissals (
    symbol     TEXT NOT NULL,
    day        TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (symbol, day)
);
"#;

/// Idempotent column additions for DBs created before the column existed.
/// `ALTER TABLE … ADD COLUMN` errors if the column is already present, so the
/// error is swallowed — the column simply already exists.
const ALTERS: &[&str] = &[
    "ALTER TABLE price_alarms ADD COLUMN triggered_at TEXT",
    "ALTER TABLE mean_reversion_scores ADD COLUMN prev_volume INTEGER",
    "ALTER TABLE mean_reversion_scores ADD COLUMN parabolic_score REAL NOT NULL DEFAULT 0",
    "ALTER TABLE mean_reversion_scores ADD COLUMN volume_score REAL NOT NULL DEFAULT 0",
    "ALTER TABLE mean_reversion_scores ADD COLUMN run_score REAL NOT NULL DEFAULT 0",
    "ALTER TABLE mean_reversion_scores ADD COLUMN run_len INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE mean_reversion_scores ADD COLUMN run_dir INTEGER NOT NULL DEFAULT 0",
    // Multi-day close-to-close % change (1d/2d/4d/6d; 3d/5d predate this and are
    // already in the CREATE for older DBs).
    "ALTER TABLE fundamentals_cache ADD COLUMN change_1d_pct REAL",
    "ALTER TABLE fundamentals_cache ADD COLUMN change_2d_pct REAL",
    "ALTER TABLE fundamentals_cache ADD COLUMN change_4d_pct REAL",
    "ALTER TABLE fundamentals_cache ADD COLUMN change_6d_pct REAL",
];

pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(MIGRATIONS)?;
    for stmt in ALTERS {
        let _ = conn.execute(stmt, []); // ignore "duplicate column name"
    }
    Ok(())
}
