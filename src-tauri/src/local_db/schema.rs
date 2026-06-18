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
    -- Behavioural scores computed once at startup over the daily cache, stored as a
    -- raw metric + a DB-wide percentile rank (0..100, 100 = worst). See
    -- `cache_repository::recompute_pump_dump_scores` / `recompute_dilution_scores`.
    pump_dump_raw    REAL,   -- mean(wick/ATR) × (1 + big-wick frequency)
    pump_dump_score  REAL,   -- percentile rank of pump_dump_raw (100 = most pump&dump)
    dilution_pct_12m REAL,   -- split-adjusted shares-outstanding change over ~12 months
    dilution_score   REAL,   -- percentile rank of dilution_pct_12m (100 = most dilutive)
    shares_outstanding_12m REAL,  -- split-adjusted shares ~12 months ago (transparency)
    -- Absolute per-ticker risk scores (0..100, computed from already-collected
    -- SEC filings / financials / short interest; NULL = inputs not collected).
    dilution_capacity_score REAL,  -- legal/filing readiness to dilute fast (SEC shelf/forms/flags)
    dilution_need_score     REAL,  -- apparent need for cash (losses, burn, runway)
    short_interest_score    REAL,  -- short crowding / squeeze fuel (short%float + days-to-cover)
    -- Most recent split (rolled up from `ticker_splits`) + count over the last year.
    last_split_date  TEXT,
    last_split_label TEXT,
    split_count_1y   INTEGER,
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
-- Panic Mean Reversion pre-open watchlist, rebuilt once per trading day at 09:00 ET
-- (see `crate::scoring::build_and_store` / `crate::panic_watchlist`). Each row is a
-- ticker retained by one of two rankings over a premarket-liquidity-filtered
-- universe: "BB" = cumulative soft-Bollinger area (BBZ excess beyond 1.7σ summed
-- over 6 days), "MA" = move since the last SMA20 contact normalised by ATR20. The
-- top 10 of each list are merged (a ticker kept once, in its better-ranked list);
-- display_score interleaves the lists 1-for-1 by rank for the screener ordering.
CREATE TABLE IF NOT EXISTS panic_watchlist (
    symbol        TEXT PRIMARY KEY NOT NULL,
    list_kind     TEXT NOT NULL DEFAULT 'BB',   -- 'BB' | 'MA'
    value         REAL NOT NULL DEFAULT 0,       -- BB area sum, or |move|/ATR20
    direction     INTEGER NOT NULL DEFAULT 0,    -- +1 up / −1 down / 0
    rank          INTEGER NOT NULL DEFAULT 0,    -- 1-based rank within its list
    display_score REAL NOT NULL DEFAULT 0,       -- global interleaved ordering key
    prev_volume   INTEGER,
    updated_at    TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_panic_watchlist_display ON panic_watchlist(display_score DESC);

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
    -- 'intraday' drawings show only on intraday panes, 'daily' only on the daily
    -- pane — a drawing belongs to the timeframe class it was placed on.
    scope      TEXT NOT NULL DEFAULT 'intraday',
    -- Style (TradingView-like editing). NULL = use the rendering defaults.
    color      TEXT,
    opacity    REAL,
    width      REAL,
    line_style TEXT,
    font_size  REAL,
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

-- Historical shares-outstanding snapshots per ticker (SEC XBRL frames, bulk:
-- dei:EntityCommonStockSharesOutstanding). Decoupled evidence feeding the rolled-up
-- dilution metrics in `fundamentals_cache`. One row per (symbol, period_end).
CREATE TABLE IF NOT EXISTS dilution_snapshots (
    symbol             TEXT NOT NULL,
    period_end         TEXT NOT NULL,    -- YYYY-MM-DD of the XBRL instant frame
    shares_outstanding REAL NOT NULL,    -- as-reported (NOT split-adjusted)
    updated_at         TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (symbol, period_end)
);
CREATE INDEX IF NOT EXISTS idx_dilution_snapshots_symbol ON dilution_snapshots(symbol, period_end DESC);

-- Stock split events per ticker (Alpaca corporate-actions, bulk). Feeds the
-- rolled-up split columns in `fundamentals_cache` and the split-neutralisation of
-- the dilution score. One row per (symbol, ex_date).
CREATE TABLE IF NOT EXISTS ticker_splits (
    symbol      TEXT NOT NULL,
    ex_date     TEXT NOT NULL,           -- YYYY-MM-DD
    label       TEXT,                    -- "x4" forward, "1:10" reverse
    from_factor REAL,                    -- old_rate
    to_factor   REAL,                    -- new_rate (split factor = to/from)
    updated_at  TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (symbol, ex_date)
);
CREATE INDEX IF NOT EXISTS idx_ticker_splits_symbol ON ticker_splits(symbol, ex_date DESC);

-- ─── Company intelligence ────────────────────────────────────────────────────
-- Normalized "company intelligence" per ticker, collected by the isolated
-- `crate::company_intel` job (SEC EDGAR / Massive / FMP). This is PURELY ADDITIVE
-- data — it never touches float / fundamentals (those live in fundamentals_cache
-- and universe_assets and are owned by the startup pipeline).
--
-- Each logical section (short interest, financial health, dilution, ownership)
-- carries its OWN source label + updated_at marker, so a partial collection (one
-- provider down) only refreshes the sections it could reach and the rest keeps the
-- last good value. `last_updated_at` is the overall cache marker used for TTL.
CREATE TABLE IF NOT EXISTS company_intel (
    symbol                       TEXT PRIMARY KEY NOT NULL,
    -- Resolved SEC CIK (10-digit, zero-padded), cached so later runs skip the
    -- ticker→CIK lookup.
    cik                          TEXT,

    -- Short interest (source: Massive).
    short_interest               INTEGER,
    days_to_cover                REAL,
    short_interest_settlement    TEXT,
    short_interest_source        TEXT,
    short_interest_updated_at    TEXT,

    -- Financial health (source: SEC Company Facts XBRL, FMP fallback).
    net_income_last_q            REAL,
    net_income_ttm               REAL,
    negative_quarters_last4      INTEGER,
    operating_cash_flow_ttm      REAL,
    cash_and_equivalents         REAL,
    financials_period_end        TEXT,
    financials_source            TEXT,
    financials_updated_at        TEXT,

    -- Registered S-3 / dilution filings (source: SEC EDGAR submissions). The raw
    -- filings live in `company_filings`; these are the rolled-up summary fields.
    has_recent_shelf             INTEGER,   -- 0/1: an S-3 family filing seen recently
    latest_dilution_form         TEXT,
    latest_dilution_date         TEXT,
    -- JSON: { "atm": bool, "resale": bool, "warrants": bool, "offering_amount": f64|null }
    dilution_flags               TEXT,
    dilution_source              TEXT,
    dilution_updated_at          TEXT,

    -- Ownership / locked shares (source: SEC 13D/13G; FMP/Massive fallback).
    institutional_ownership_pct  REAL,
    insider_ownership_pct        REAL,
    holders_5pct_count           INTEGER,
    -- JSON array: [{ "name": str, "pct": f64|null, "form": str, "date": str }]
    holders_5pct                 TEXT,
    restricted_shares            INTEGER,
    ownership_source             TEXT,
    ownership_updated_at         TEXT,

    -- Overall cache markers.
    last_updated_at              TEXT NOT NULL DEFAULT (datetime('now')),
    -- Last collection error per section, JSON: { "short_interest": "…", … }.
    last_errors                  TEXT
);

-- Raw recent SEC filings fetched for a ticker (the dilution / ownership feed).
-- One row per (symbol, accession_number). Kept separate from the normalized
-- `company_intel` table so the rolled-up summary and the underlying evidence are
-- decoupled — the UI can drill from a dilution flag down to the actual filing.
CREATE TABLE IF NOT EXISTS company_filings (
    accession_number  TEXT NOT NULL,
    symbol            TEXT NOT NULL,
    cik               TEXT,
    form_type         TEXT NOT NULL,
    filing_date       TEXT,
    report_date       TEXT,
    primary_document  TEXT,
    document_url      TEXT,
    description       TEXT,
    -- Classification computed at fetch time.
    category          TEXT,      -- 'dilution' | 'ownership' | 'other'
    detected_atm      INTEGER NOT NULL DEFAULT 0,
    detected_resale   INTEGER NOT NULL DEFAULT 0,
    detected_warrants INTEGER NOT NULL DEFAULT 0,
    offering_amount   REAL,
    fetched_at        TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (symbol, accession_number)
);
CREATE INDEX IF NOT EXISTS idx_company_filings_symbol ON company_filings(symbol, filing_date DESC);
CREATE INDEX IF NOT EXISTS idx_company_filings_category ON company_filings(symbol, category);
"#;

/// Idempotent column additions for DBs created before the column existed.
/// `ALTER TABLE … ADD COLUMN` errors if the column is already present, so the
/// error is swallowed — the column simply already exists.
const ALTERS: &[&str] = &[
    "ALTER TABLE price_alarms ADD COLUMN triggered_at TEXT",
    // Panic Mean Reversion was reworked into a two-list watchlist (`panic_watchlist`);
    // the old composite-scoring table is obsolete. Dropped so a stale schema can't
    // linger. (Not strictly an ALTER, but the same idempotent-migration channel.)
    "DROP TABLE IF EXISTS mean_reversion_scores",
    // Multi-day close-to-close % change (1d/2d/4d/6d; 3d/5d predate this and are
    // already in the CREATE for older DBs).
    "ALTER TABLE fundamentals_cache ADD COLUMN change_1d_pct REAL",
    "ALTER TABLE fundamentals_cache ADD COLUMN change_2d_pct REAL",
    "ALTER TABLE fundamentals_cache ADD COLUMN change_4d_pct REAL",
    "ALTER TABLE fundamentals_cache ADD COLUMN change_6d_pct REAL",
    // Behavioural scores + rolled-up splits (added after the fundamentals_cache
    // CREATE above; ALTERs let pre-existing DBs gain the columns idempotently).
    "ALTER TABLE fundamentals_cache ADD COLUMN pump_dump_raw REAL",
    "ALTER TABLE fundamentals_cache ADD COLUMN pump_dump_score REAL",
    "ALTER TABLE fundamentals_cache ADD COLUMN dilution_pct_12m REAL",
    "ALTER TABLE fundamentals_cache ADD COLUMN dilution_score REAL",
    "ALTER TABLE fundamentals_cache ADD COLUMN shares_outstanding_12m REAL",
    "ALTER TABLE fundamentals_cache ADD COLUMN last_split_date TEXT",
    "ALTER TABLE fundamentals_cache ADD COLUMN last_split_label TEXT",
    "ALTER TABLE fundamentals_cache ADD COLUMN split_count_1y INTEGER",
    "ALTER TABLE fundamentals_cache ADD COLUMN dilution_capacity_score REAL",
    "ALTER TABLE fundamentals_cache ADD COLUMN dilution_need_score REAL",
    "ALTER TABLE fundamentals_cache ADD COLUMN short_interest_score REAL",
    // Editable drawings: per-timeframe scope + style columns (TradingView-like).
    "ALTER TABLE chart_drawings ADD COLUMN scope TEXT NOT NULL DEFAULT 'intraday'",
    "ALTER TABLE chart_drawings ADD COLUMN color TEXT",
    "ALTER TABLE chart_drawings ADD COLUMN opacity REAL",
    "ALTER TABLE chart_drawings ADD COLUMN width REAL",
    "ALTER TABLE chart_drawings ADD COLUMN line_style TEXT",
    "ALTER TABLE chart_drawings ADD COLUMN font_size REAL",
];

pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(MIGRATIONS)?;
    for stmt in ALTERS {
        let _ = conn.execute(stmt, []); // ignore "duplicate column name"
    }
    Ok(())
}
