// SQLite schema, in TWO database files opened as ONE connection (`local_db::open`):
//
//   main = <root>/cache/cache.db   re-downloadable market / reference data
//   user = <root>/user/user.db     personal data, attached as schema `user`
//
// SQLite resolves an unqualified table name in `main` first, then in the attached
// databases. A personal table exists ONLY in `user`, so every query in the code
// base keeps naming it bare (`FROM executions`), and joins or transactions across
// the two files work as they did with one. Only the DDL below says where a table
// lives: `user.` on the personal ones. That prefix is not optional on an index —
// an unqualified CREATE INDEX on an attached table is an error.
//
// WHAT IS PERSONAL: what the person using this install wrote, did or paid for, and
// that no provider can hand back — trades and their notes, the review, drawings,
// alarms, settings, outbound queues, the usage log, model verdicts. Everything a
// machine can rebuild from its providers is cache. `USER_TABLES` is the list, and
// `crate::user_data` packs exactly that.
//
// Every statement is idempotent (IF NOT EXISTS): this runs on every startup and
// never drops data.

use rusqlite::{Connection, Result};

/// Schema name the personal database is attached under.
pub const USER_SCHEMA_NAME: &str = "user";

/// Version of the personal schema, stamped into every data pack. Bump it when a
/// change makes a pack unreadable by OLDER builds (a new column does not: `ALTERS`
/// brings an older pack forward). A pack from a newer version is refused.
pub const USER_SCHEMA_VERSION: u32 = 1;

/// Every table of the personal database. The single source of truth for the split
/// of a pre-separation database (`super::split`) and for what a data pack holds.
pub const USER_TABLES: &[&str] = &[
    "user_config",
    "ui_state",
    // Trades
    "executions",
    "trade_levels",
    "fill_timing",
    "journal_entries",
    "screenshot_files",
    "broker_runtime_state",
    // Charts
    "chart_drawings",
    "price_alarms",
    // Screener
    "screener_pins",
    "screener_ratings",
    "screener_dismissals",
    // Review
    "review_sessions",
    "review_voice",
    "review_dispatch",
    "review_log",
    "review_outbox",
    "attention_snapshots",
    // TradeTally
    "tradetally_sync_queue",
    "tradetally_trade_ids",
    "diary_entries_local",
    // Model verdicts
    "llm_analysis",
    "panic_llm_scores",
    "micro_quality_scores",
    "micro_pullback_quality_scores",
    // Misc
    "bug_reports",
    "usage_events",
];

/// Keys of the key/value bag that are personal settings or state rather than cache
/// bookkeeping. `cache_repository::{get,set}_app_meta` route on this list, so a
/// call site never has to know which file its key lives in.
///
/// A NEW personal key must be added here, or it lands in the cache database and
/// stays behind when the data moves.
pub const USER_CONFIG_KEYS: &[&str] = &[
    // Strategies: on/off, risk, and each engine's own options.
    "strategy_overrides",
    "strategy_risk_overrides",
    "micro_pullback_v1_options",
    "micro_pullback_options", // legacy key Dislocation still reads once
    "dislocation_options",
    "backside_options",
    "apex_v1_options",
    "dip_v1_options",
    // « Attention du marché » screen.
    "attention_map_v1_options",
    "attention_window_v1",
    // Simulated broker book + per-ticker SL / TP / trade id.
    "internal_book_v1",
    "chart_contexts_v1",
    // "The bundled defaults were copied into the user folder" marker.
    "default_assets_seeded",
];

const PRAGMAS: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA user.journal_mode = WAL;
PRAGMA foreign_keys = ON;
"#;

// ─── Personal database (schema `user`) ──────────────────────────────────────────

const USER_SCHEMA: &str = r#"
-- Personal settings and state as key/value: strategy switches and risk, each
-- engine's options blob, the simulated book. Keys: `USER_CONFIG_KEYS`.
CREATE TABLE IF NOT EXISTS user.user_config (
    key        TEXT PRIMARY KEY NOT NULL,
    value      TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Interface state the frontend persists (dashboard layout and card surfaces,
-- hotkeys, gamepad layout, chart theme, pane sizes…): one row per store, the
-- value being that store's JSON. It used to live in the webview's localStorage,
-- which belongs to one machine AND one build flavour (dev and release are two
-- origins), and could not travel.
CREATE TABLE IF NOT EXISTS user.ui_state (
    key        TEXT PRIMARY KEY NOT NULL,
    value      TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Outbound event queue for TradeTally. Drained by a background worker.
-- Status: 'pending' | 'success' | 'failed'
CREATE TABLE IF NOT EXISTS user.tradetally_sync_queue (
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
CREATE INDEX IF NOT EXISTS user.idx_sync_queue_status ON tradetally_sync_queue(status);
CREATE INDEX IF NOT EXISTS user.idx_sync_queue_created ON tradetally_sync_queue(created_at DESC);

-- Per-Alpaca-account runtime state. Open broker trades and extended-hours
-- protection plans must survive process restarts and account switching: positions
-- routinely stay open for several sessions/days.
CREATE TABLE IF NOT EXISTS user.broker_runtime_state (
    account_id      TEXT PRIMARY KEY NOT NULL,
    journal_json    TEXT,
    protection_json TEXT,
    updated_at      TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Local paths of captured screenshots.
CREATE TABLE IF NOT EXISTS user.screenshot_files (
    id         TEXT PRIMARY KEY NOT NULL,
    trade_id   TEXT,
    path       TEXT NOT NULL,
    uploaded   INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Per-trade journal entries (notes, confidence, tags). Upserted when saved.
CREATE TABLE IF NOT EXISTS user.journal_entries (
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
CREATE TABLE IF NOT EXISTS user.tradetally_trade_ids (
    local_trade_id  TEXT PRIMARY KEY NOT NULL,
    tt_trade_id     TEXT NOT NULL,
    created_at      TEXT NOT NULL DEFAULT (datetime('now'))
);

-- User-submitted bug reports. Persist across launches.
-- priority: 1 = low, 2 = medium, 3 = high.
CREATE TABLE IF NOT EXISTS user.bug_reports (
    id         TEXT PRIMARY KEY NOT NULL,
    text       TEXT NOT NULL,
    priority   INTEGER NOT NULL DEFAULT 2,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS user.idx_bug_reports_sort ON bug_reports(priority DESC, created_at DESC);

-- Price alarms placed on charts. Each alarm is tied to a ticker (and the
-- strategy of the zone/chart it was drawn from). Triggering is not wired yet —
-- this just persists the level so it can be armed later.
CREATE TABLE IF NOT EXISTS user.price_alarms (
    id           TEXT PRIMARY KEY NOT NULL,
    symbol       TEXT NOT NULL,
    strategy_id  TEXT,
    price        REAL NOT NULL,
    created_at   TEXT NOT NULL DEFAULT (datetime('now')),
    -- Set the moment the price crosses the alarm level (fires an Open alert).
    -- NULL = armed/waiting. Watching is done in the scanner loop.
    triggered_at TEXT
);
CREATE INDEX IF NOT EXISTS user.idx_price_alarms_symbol ON price_alarms(symbol);

-- Tickers the user PINNED in the pre-open screener. Unlike dismissals this is NOT
-- day-scoped: a pin persists across days until the user removes it. `simulated`
-- separates pins placed during a Market Replay from live ones so a backtest never
-- pollutes the real watchlist (and vice versa).
CREATE TABLE IF NOT EXISTS user.screener_pins (
    symbol     TEXT NOT NULL,
    simulated  INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (symbol, simulated)
);

-- User's own 1..3 rating of a pre-open ticker's potential. A private memo aid:
-- nothing reads it but the card that draws it — no score, no filter, no ordering.
-- Like a pin it has NO day column, so a rating placed today is still there next
-- week; `simulated` keeps backtest ratings out of the live list. Rating 0 is not
-- stored — clearing deletes the row.
CREATE TABLE IF NOT EXISTS user.screener_ratings (
    symbol     TEXT NOT NULL,
    simulated  INTEGER NOT NULL DEFAULT 0,
    rating     INTEGER NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (symbol, simulated)
);

-- Persisted LLM analysis RESULTS per ticker (panic mean-reversion button read).
-- Only the model's outputs are stored (context summary + reversion verdict), not
-- the prompt or the fetched news/OHLC. Every call is appended (history); the UI
-- hydrates the most recent row for a symbol when a zone re-opens.
CREATE TABLE IF NOT EXISTS user.llm_analysis (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    symbol      TEXT NOT NULL,
    strategy_id TEXT NOT NULL,
    context     TEXT,
    verdict     TEXT,
    created_at  TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS user.idx_llm_analysis_symbol ON llm_analysis(symbol, created_at DESC);

-- Panic Score V1: the LLM's read of each pre-open watchlist ticker (see
-- `crate::panic_llm`). One row per (symbol, day, mode), written only when the user
-- presses the analysis button — nothing here is ever produced automatically.
--
-- Scoped by `day` AND `simulated`, exactly like screener_pins / panic_watchlist_v2:
-- an analysis run during a Market Replay must never surface in live mode, nor the
-- reverse. Since the LLM ranking IS the definitive ranking, letting a backtest's
-- scores leak into the live watchlist would silently reorder a real trading morning.
--
-- `panic_score_raw` keeps the per-ticker score BEFORE the calibration pass while
-- `panic_score` holds the value actually used for ranking. Storing both is what makes
-- calibration auditable: a large systematic gap between the two columns means the
-- rubric is drifting and needs re-anchoring, and that is invisible if only the final
-- score survives. `rubric_version` pins which barème produced the row, so scores from
-- two different rubrics are never silently compared.
CREATE TABLE IF NOT EXISTS user.panic_llm_scores (
    symbol             TEXT NOT NULL,
    day                TEXT NOT NULL,
    simulated          INTEGER NOT NULL DEFAULT 0,
    panic_score        REAL,
    panic_score_raw    REAL,
    confidence_score   REAL,
    catalyst_strength  REAL,
    catalyst_label     TEXT,
    catalyst_type      TEXT,
    reversibility      TEXT,
    one_liner          TEXT,
    key_drivers        TEXT,   -- JSON array of short strings
    forced_flow        TEXT,
    euphoria           TEXT,
    normalization_prob REAL,
    data_quality       TEXT,   -- JSON: feed, missing + look-ahead fields
    web_used           INTEGER NOT NULL DEFAULT 0,
    web_provider       TEXT,
    adjustment_reason  TEXT,
    rubric_version     TEXT,
    model              TEXT,
    ranking_score      REAL,
    trade_direction    TEXT,
    timing_state       TEXT,
    fundamental_veto   INTEGER NOT NULL DEFAULT 0,
    veto_reason        TEXT,
    eligibility        TEXT,
    catalyst_direction TEXT,
    created_at         TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (symbol, day, simulated)
);
CREATE INDEX IF NOT EXISTS user.idx_panic_llm_day
    ON panic_llm_scores(day, simulated, panic_score DESC);

-- Micro Pullback "Qualité A+" judgements. Keyed by (symbol, day, SESSION, simulated):
-- the engine's lock is per session, so one ticker can legitimately be judged twice in
-- a day — premarket and again at the open — on two different moves. Keying on the day
-- alone would let the second silently overwrite the first.
-- `pass1_score` (news scoring) and `pass2_score` (deep analysis) are kept side by side
-- so it stays answerable, weeks later, whether the deep pass ever changed an outcome.
CREATE TABLE IF NOT EXISTS user.micro_quality_scores (
    symbol          TEXT NOT NULL,
    day             TEXT NOT NULL,
    session         TEXT NOT NULL,
    simulated       INTEGER NOT NULL DEFAULT 0,
    pass1_score     REAL,
    pass2_score     REAL,
    final_score     REAL,
    threshold       REAL,
    passed          INTEGER NOT NULL DEFAULT 0,
    adjustments     TEXT,   -- human-readable term-by-term audit trail
    confidence      REAL,
    sustain_prob    REAL,
    news_verdict    TEXT,
    news_freshness  TEXT,
    catalyst_type   TEXT,
    catalyst_label  TEXT,
    dilution_risk   TEXT,
    dilution_read   TEXT,
    financial_read  TEXT,
    red_flags       TEXT,   -- JSON array
    key_drivers     TEXT,   -- JSON array
    one_liner       TEXT,
    entry_note      TEXT,
    tempo_depth     TEXT,   -- JSON array of tempo seconds
    rejection       TEXT,   -- why it was turned away, NULL when it passed
    rubric_version  TEXT,
    model           TEXT,
    -- MARKET time of the verdict (replay clock in a replay), not wall time. The UI
    -- compares it with a headline's publication time to tell "already scored with
    -- this news" from "a headline landed after the score" — `created_at` cannot do
    -- that job because it is wall time and a replay's two clocks disagree.
    scored_at       TEXT,
    rvol_score      REAL,
    attention_score REAL,
    news_score      REAL,
    created_at      TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (symbol, day, session, simulated)
);
CREATE INDEX IF NOT EXISTS user.idx_micro_quality_day
    ON micro_quality_scores(day, simulated, final_score DESC);

-- Micro Pullback's own news-scoring table. Deliberately separate from
-- micro_quality_scores (Dislocation's) rather than a shared table keyed by an added
-- strategy_id: the two engines already keep their runtime options under distinct
-- app_meta keys for the same reason (see micro_pullback::options header) — sharing a
-- table would mean rebuilding its primary key to avoid a symbol/day/session collision
-- between the two strategies, for no benefit over just not sharing it.
CREATE TABLE IF NOT EXISTS user.micro_pullback_quality_scores (
    symbol      TEXT NOT NULL,
    day         TEXT NOT NULL,
    session     TEXT NOT NULL,
    simulated   INTEGER NOT NULL DEFAULT 0,
    score       REAL,
    verdict     TEXT,
    freshness   TEXT,
    red_flags   TEXT,   -- JSON array
    one_liner   TEXT,
    model       TEXT,
    scored_at   TEXT,
    created_at  TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (symbol, day, session, simulated)
);
CREATE INDEX IF NOT EXISTS user.idx_micro_pullback_quality_day
    ON micro_pullback_quality_scores(day, simulated, score DESC);

-- Trade executions (internal simulated fills), persisted per ticker so the chart
-- can draw the entry/scale/exit triangles even days later / across restarts.
-- `quantity` is the SIGNED share delta of the fill (+ buy/long, − sell/short),
-- so the running position and realized P&L are reconstructable from the rows.
CREATE TABLE IF NOT EXISTS user.executions (
    fill_id    TEXT PRIMARY KEY NOT NULL,
    trade_id   TEXT NOT NULL,
    symbol     TEXT NOT NULL,
    quantity   INTEGER NOT NULL,
    fill_price REAL NOT NULL,
    filled_at  TEXT NOT NULL,
    -- Market time of the price the fill executed against. On a delayed data plan
    -- it trails `filled_at` by the plan's delay; the chart anchors its triangle
    -- here so the marker sits on the candle that produced the price.
    data_at    TEXT,
    -- Sizing button behind an entry (25 / 50 / 100). NULL for exits.
    size_pct   INTEGER
);
CREATE INDEX IF NOT EXISTS user.idx_executions_symbol ON executions(symbol);

-- Original (launch-time) stop loss / take profit per trade. Recorded ONCE on the
-- first fill and never overwritten, so they survive later SL/TP modifications —
-- they're the levels the journal/R:R is based on. Drive the thin "original SL"
-- (red) and "original TP" (green) segments drawn on every chart of the symbol
-- for the duration of the trade.
CREATE TABLE IF NOT EXISTS user.trade_levels (
    trade_id    TEXT PRIMARY KEY NOT NULL,
    symbol      TEXT NOT NULL,
    original_sl REAL,
    original_tp REAL,
    created_at  TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS user.idx_trade_levels_symbol ON trade_levels(symbol);

-- User chart drawings (trend lines + text annotations), persisted PER TICKER so
-- they reappear on every chart/zone showing that symbol and survive restarts.
-- `kind`='line' uses both points (t1,p1)-(t2,p2); 'text' uses (t1,p1)+text.
-- Times are chart Unix-seconds, prices are raw. Pixel fallbacks aren't stored —
-- positions are recomputed from time/price on render.
CREATE TABLE IF NOT EXISTS user.chart_drawings (
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
CREATE INDEX IF NOT EXISTS user.idx_chart_drawings_symbol ON chart_drawings(symbol);

-- Pre-open screener cards the user dismissed. Scoped to a trading DAY (ET date)
-- so a dismissal persists across restarts for the rest of the day, then the
-- ticker can reappear the next day. Old days are pruned on read.
CREATE TABLE IF NOT EXISTS user.screener_dismissals (
    symbol     TEXT NOT NULL,
    day        TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (symbol, day)
);

-- Local copy of diary (journal) entries sent to TradeTally from the dashboard's
-- Journal card. Mirror only — the authoritative entry lives in TradeTally
-- (POST /api/diary, create-or-update per ET day). Kept for a future history view.
CREATE TABLE IF NOT EXISTS user.diary_entries_local (
    id         TEXT PRIMARY KEY NOT NULL,
    entry_date TEXT NOT NULL,
    title      TEXT NOT NULL DEFAULT '',
    content    TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- ── Trade review (onglet Revue) — see `crate::review` ─────────────────────────
-- One recording session per (ET day, ticker, mode): opened when a reviewed
-- strategy first shows the ticker (or on demand for a missed-trade note), closed
-- on « Libérer ». Its market data lives in its own SQLite file (`store_path`).
-- The session id doubles as the "trade id" of a missed trade: journal notes and
-- screenshots filed against it stay local (never reach TradeTally).
-- All *_ms columns are epoch milliseconds UTC, on the MARKET clock (simulated
-- during a Market Replay).
CREATE TABLE IF NOT EXISTS user.review_sessions (
    session_id  TEXT PRIMARY KEY NOT NULL,
    day         TEXT NOT NULL,
    symbol      TEXT NOT NULL,
    mode        TEXT NOT NULL,          -- live | replay | backfill
    strategy_id TEXT,
    setup       TEXT,
    alert_price REAL,
    opened_ms   INTEGER NOT NULL,
    closed_ms   INTEGER,
    status      TEXT NOT NULL DEFAULT 'open',   -- open | closed
    data_source TEXT,                   -- comma list of origins captured
    enriched_ms INTEGER,                -- last SIP REST rebuild (real clock)
    store_path  TEXT NOT NULL,
    created_at  TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS user.idx_review_sessions_day_sym ON review_sessions(day, symbol);

-- One voice recording attached to a session. The audio is archived as 16 kHz
-- mu-law with the silences cut out; `chunks_json` maps each voiced chunk back to
-- the market clock so the replay plays it (and its subtitles) at the right time.
CREATE TABLE IF NOT EXISTS user.review_voice (
    voice_id        TEXT PRIMARY KEY NOT NULL,
    session_id      TEXT NOT NULL,
    start_ms        INTEGER NOT NULL,
    end_ms          INTEGER NOT NULL,
    speed           REAL NOT NULL DEFAULT 1.0,   -- replay speed while recording
    audio_path      TEXT,
    chunks_json     TEXT NOT NULL DEFAULT '[]',
    transcript_json TEXT,
    cleaned_text    TEXT,
    created_at      TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS user.idx_review_voice_session ON review_voice(session_id);

-- Which session item (typed note, voice note, screenshot) was already copied onto
-- which trade of the session — the copy happens once per (item, trade).
CREATE TABLE IF NOT EXISTS user.review_dispatch (
    item_kind TEXT NOT NULL,
    item_id   TEXT NOT NULL,
    trade_id  TEXT NOT NULL,
    at        TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (item_kind, item_id, trade_id)
);

-- Every completed review (« Suivant »). A trade can be reviewed many times.
CREATE TABLE IF NOT EXISTS user.review_log (
    review_id   TEXT PRIMARY KEY NOT NULL,
    target_kind TEXT NOT NULL,          -- tt | missed
    target_id   TEXT NOT NULL,          -- tt_id or session_id
    review_date TEXT NOT NULL,          -- ET date of the review
    reviewed_at TEXT NOT NULL,
    text        TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS user.idx_review_log_target ON review_log(target_kind, target_id);

-- What the review wrote onto TradeTally trades and has not reached the server yet
-- (offline, TradeTally down). Operations, folded over the server's notes at send
-- time — see `crate::review::outbox`. A row leaves once TradeTally accepted it.
CREATE TABLE IF NOT EXISTS user.review_outbox (
    op_id       INTEGER PRIMARY KEY AUTOINCREMENT,
    tt_id       TEXT NOT NULL,
    op          TEXT NOT NULL,          -- set_notes | append
    text        TEXT NOT NULL,          -- the new text | the review block
    local_notes TEXT NOT NULL,          -- trader's text locally after this op
    created_ms  INTEGER NOT NULL,
    attempts    INTEGER NOT NULL DEFAULT 0,
    last_error  TEXT
);
CREATE INDEX IF NOT EXISTS user.idx_review_outbox_tt ON review_outbox(tt_id);

-- Millisecond timing of every fill, both brokers. TradeTally cannot hold these;
-- they are TagDash's own, kept for exact replay markers and, later, latency work
-- (order sent → broker ack → fill → fill received).
CREATE TABLE IF NOT EXISTS user.fill_timing (
    fill_id   TEXT PRIMARY KEY NOT NULL,
    trade_id  TEXT NOT NULL,
    symbol    TEXT NOT NULL,
    broker    TEXT NOT NULL,            -- tagdash | alpaca
    quantity  INTEGER NOT NULL,         -- signed (+ buy, - sell)
    price     REAL NOT NULL,
    sent_ms   INTEGER,
    ack_ms    INTEGER,
    filled_ms INTEGER NOT NULL,
    recv_ms   INTEGER NOT NULL,         -- real clock
    data_ms   INTEGER
);
CREATE INDEX IF NOT EXISTS user.idx_fill_timing_symbol ON fill_timing(symbol, filled_ms);
CREATE INDEX IF NOT EXISTS user.idx_fill_timing_trade ON fill_timing(trade_id);

-- What the journal keeps: per day, only the tickers that made the map (their
-- slots and their medians) plus the per-slot weather. Enough to redraw the screen
-- as it stood at any instant of that day, with no provider call.
CREATE TABLE IF NOT EXISTS user.attention_snapshots (
    day        TEXT PRIMARY KEY NOT NULL,
    prev_day   TEXT NOT NULL,
    covered_ms INTEGER NOT NULL,
    tickers    INTEGER NOT NULL,
    payload    BLOB NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- ── Usage log — see `usage_repository` ────────────────────────────────────────
-- One row per user action, raw: which control or command, from where, and when.
-- `ts_ms` is the real clock. The two minute-of-day columns answer "at what time
-- of day" directly — in market time and on the user's own clock — without
-- timezone math in SQL.
CREATE TABLE IF NOT EXISTS user.usage_events (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    ts_ms        INTEGER NOT NULL,
    et_date      TEXT NOT NULL,              -- YYYY-MM-DD, US Eastern
    et_minute    INTEGER NOT NULL,           -- 0..1439, US Eastern
    local_minute INTEGER NOT NULL,           -- 0..1439, the machine's wall clock
    source       TEXT NOT NULL,              -- click | change | hotkey | gamepad | key
    action       TEXT NOT NULL,              -- command id, or the control's label
    context      TEXT NOT NULL,              -- region > dialog > tab > menu
    input        TEXT,                       -- element kind, key chord, pad layer:button
    hint         TEXT,                       -- the control's tooltip, when it adds to the label
    view         TEXT NOT NULL,              -- top-level view (+ trading session)
    window       TEXT NOT NULL,              -- main | attention
    replay       INTEGER NOT NULL DEFAULT 0, -- 1 = a Market Replay was running
    app_version  TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS user.idx_usage_events_action ON usage_events(source, action);
CREATE INDEX IF NOT EXISTS user.idx_usage_events_day ON usage_events(et_date, et_minute);
"#;

// ─── Cache database (schema `main`) ─────────────────────────────────────────────

const CACHE_SCHEMA: &str = r#"
-- Key-value bag for CACHE bookkeeping: fetch-date markers, probed data plans,
-- "synced at" stamps. Personal settings live in `user.user_config` — the split
-- is by key, see `USER_CONFIG_KEYS`.
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
    security_type TEXT,                  -- Massive reference code: CS, ETF, ETN…
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

-- Premarket (04:00-09:30 ET) daily candles, folded from 15-minute intraday bars
-- by `crate::premarket`. Both providers' daily bars are regular-session only, so
-- this is the ONLY place the premarket range of a past day exists. A closed day's
-- premarket is immutable, so rows are written once and never refreshed.
CREATE TABLE IF NOT EXISTS premarket_daily (
    symbol      TEXT NOT NULL,
    date        TEXT NOT NULL,
    open        REAL NOT NULL,
    high        REAL NOT NULL,
    low         REAL NOT NULL,
    close       REAL NOT NULL,
    volume      INTEGER NOT NULL DEFAULT 0,
    vwap        REAL,
    trade_count INTEGER,
    updated_at  TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (symbol, date)
);

-- Which date span of `premarket_daily` has actually been fetched for a symbol.
-- Load-bearing: a day on which the symbol never printed in premarket produces NO
-- row, so "no row" cannot be read as "not fetched yet" — without this table an
-- illiquid name would be re-fetched on every single chart open.
CREATE TABLE IF NOT EXISTS premarket_coverage (
    symbol     TEXT PRIMARY KEY NOT NULL,
    first_date TEXT NOT NULL,
    last_date  TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Tags retrieved from TradeTally API at startup.
CREATE TABLE IF NOT EXISTS tradetally_tags (
    tag        TEXT PRIMARY KEY NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

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

-- Application log entries (info / warn / error).
CREATE TABLE IF NOT EXISTS local_logs (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    level      TEXT NOT NULL,
    message    TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_local_logs_created ON local_logs(created_at DESC);

-- Panic Mean Reversion pre-open watchlist, rebuilt once per trading DAY at 09:25 ET
-- (see `crate::scoring::build_and_store` / `crate::panic_watchlist::ensure_day`).
--
-- Keyed by (day, symbol) — the day is what makes a Market Replay safe: a list built
-- for the replayed date is invisible while the clock sits on any other date, so a
-- build that finishes after the clock moved degrades to "shows nothing" instead of
-- "shows the wrong day". It also lets the live list survive a replay of another day
-- untouched (no rebuild on stop).
--
-- One row = a ticker whose extension cleared the absolute thresholds. Multiday and
-- current-premarket shocks compete; the score is:
--   sigma   = |return over N days| / (σ of daily returns over the 20 days BEFORE the
--             window × √N)  — scale-free, so a $50M and a $50B name are comparable.
--   atr_exp = mean true range over the window / ATR20 measured BEFORE the window
--             — "the ATR was stable, now it has expanded".
--   score   = sigma × sqrt(atr_exp) × bounded stability/flow quality.
CREATE TABLE IF NOT EXISTS panic_watchlist_v2 (
    day         TEXT    NOT NULL,          -- ET trading day the list was built FOR
    symbol      TEXT    NOT NULL,
    score       REAL    NOT NULL,          -- extension score incl. stability/flow quality
    window_days INTEGER NOT NULL,          -- 0 = current PM; otherwise 2..5 days
    direction   INTEGER NOT NULL,          -- +1 extended up / −1 extended down
    sigma       REAL    NOT NULL,
    atr_exp     REAL    NOT NULL,
    cap_bin     TEXT,                      -- size bucket, NULL = unknown market cap
    prev_volume INTEGER,
    setup_kind  TEXT    NOT NULL DEFAULT 'multiday',
    move_pct    REAL    NOT NULL DEFAULT 0,
    stability_score REAL NOT NULL DEFAULT 0,
    volume_expansion REAL,
    directional_efficiency REAL,
    consecutive_bars INTEGER,
    updated_at  TEXT    NOT NULL,          -- REAL wall clock: write bookkeeping only
    PRIMARY KEY (day, symbol)
);
CREATE INDEX IF NOT EXISTS idx_panic_v2_rank ON panic_watchlist_v2(day, score DESC);

-- One ledger row per day the watchlist build ran. Replaces the old in-memory
-- "built_day" (which made the live list unrecoverable after replaying today's date)
-- and the `app_config['panic_watchlist_date']` marker. `row_count = 0` is a RECORDED,
-- VALID outcome: without it the builder wiped the table and re-ran the heavy job
-- every 30s forever. `retryable` separates "no survivors" (final) from "inputs were
-- missing" (retry). `simulated` + `generation` are how a replay artifact is detected.
CREATE TABLE IF NOT EXISTS panic_watchlist_build (
    day        TEXT PRIMARY KEY NOT NULL,
    built_min  INTEGER NOT NULL,           -- ET minutes on the APP clock at completion
    row_count  INTEGER NOT NULL,
    attempts   INTEGER NOT NULL DEFAULT 1,
    simulated  INTEGER NOT NULL DEFAULT 0, -- 1 = produced under a replay clock
    generation INTEGER NOT NULL DEFAULT 0, -- replay::clock::generation() at build time
    retryable  INTEGER NOT NULL DEFAULT 0,
    updated_at TEXT NOT NULL
);

-- Shares outstanding per SEC registrant, from the free XBRL `frames` API
-- (dei:EntityCommonStockSharesOutstanding). One request returns the whole market for
-- a quarter, so this is cheap to refresh. Kept per period_end rather than overwritten
-- so market cap can be computed POINT-IN-TIME during a replay (pick the newest frame
-- whose period_end is on or before the replayed day) — no look-ahead.
-- Full ticker→CIK map from https://www.sec.gov/files/company_tickers.json (one
-- request, whole market). Kept separately from `company_intel.cik`, which is only
-- populated for symbols the intel job has already processed.
CREATE TABLE IF NOT EXISTS sec_ticker_cik (
    symbol     TEXT PRIMARY KEY NOT NULL,
    cik        TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_sec_ticker_cik_cik ON sec_ticker_cik(cik);

CREATE TABLE IF NOT EXISTS sec_shares_outstanding (
    cik        TEXT NOT NULL,              -- 10-digit zero-padded
    period_end TEXT NOT NULL,              -- YYYY-MM-DD
    shares     REAL NOT NULL,
    frame      TEXT NOT NULL,              -- e.g. CY2026Q1I
    updated_at TEXT NOT NULL,
    PRIMARY KEY (cik, period_end)
);

-- Micro Pullback v2 event ledger. One row per attention/direction event sent toward
-- A+ (or served from its cache), so a silent morning can be reconstructed without
-- persisting every Top-10 refresh.
CREATE TABLE IF NOT EXISTS micro_attention_events (
    event_id             TEXT PRIMARY KEY NOT NULL,
    symbol               TEXT NOT NULL,
    market_time          TEXT NOT NULL,
    received_at          TEXT NOT NULL,
    tempo_secs           INTEGER NOT NULL,
    tempo_depth          TEXT NOT NULL,
    attention_score      REAL NOT NULL,
    direction_score      REAL NOT NULL,
    dominance_z          REAL NOT NULL,
    head_size            INTEGER NOT NULL,
    flow_rank            REAL NOT NULL,
    range_rank           REAL NOT NULL,
    relative_rank        REAL,
    tempo_rvol           REAL,
    baseline_windows     INTEGER NOT NULL DEFAULT 0,
    continuity           REAL NOT NULL,
    distribution_quality REAL NOT NULL,
    evidence_ok           INTEGER NOT NULL,
    already_alerted      INTEGER NOT NULL DEFAULT 0,
    outcome              TEXT NOT NULL,
    created_at           TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_micro_attention_market_time
    ON micro_attention_events(market_time DESC, attention_score DESC);

-- Aggregated forensic trace: at most 10 rows x 6 tempos every 30 seconds. This is
-- small enough to retain while still answering "what led the market at 04:07?"
-- without recording the whole US tape.
CREATE TABLE IF NOT EXISTS micro_attention_snapshots (
    market_time          TEXT NOT NULL,
    tempo_secs           INTEGER NOT NULL,
    rank                 INTEGER NOT NULL,
    symbol               TEXT NOT NULL,
    attention_score      REAL NOT NULL,
    direction_score      REAL NOT NULL,
    dominance_z          REAL NOT NULL,
    head_size            INTEGER NOT NULL,
    dominant             INTEGER NOT NULL,
    flow_rank            REAL NOT NULL,
    range_rank           REAL NOT NULL,
    relative_rank        REAL,
    tempo_rvol           REAL,
    baseline_windows     INTEGER NOT NULL DEFAULT 0,
    continuity           REAL NOT NULL,
    distribution_quality REAL NOT NULL,
    evidence_ok           INTEGER NOT NULL,
    already_alerted      INTEGER NOT NULL,
    received_at          TEXT NOT NULL,
    PRIMARY KEY (market_time, tempo_secs, rank)
);
CREATE INDEX IF NOT EXISTS idx_micro_attention_snap_symbol
    ON micro_attention_snapshots(symbol, market_time DESC);

-- Point-in-time Micro Pullback RVOL baseline. One target replay/live day owns a
-- profile computed strictly from minute flat files BEFORE that day. Ten-minute ET
-- buckets are fine enough to model the thin 04:00 tape without pretending a minute
-- flat file contains historical seconds; the strategy scales the expectation to
-- each 10/20/40/60/120/300-second observation window.
CREATE TABLE IF NOT EXISTS micro_rvol_baselines (
    target_day       TEXT NOT NULL,
    symbol           TEXT NOT NULL,
    et_bucket_minute INTEGER NOT NULL,
    median_volume_10m REAL NOT NULL,
    mad_volume_10m    REAL NOT NULL,
    sample_days       INTEGER NOT NULL,
    updated_at        TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (target_day, symbol, et_bucket_minute)
);
CREATE INDEX IF NOT EXISTS idx_micro_rvol_target
    ON micro_rvol_baselines(target_day, symbol);

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

-- ─── Dashboard (moodboard) ───────────────────────────────────────────────────
-- Local mirror of the user's TradeTally trades, refreshed from the API every time
-- the dashboard tab opens. TradeTally is the source of truth — this table is only
-- a cache that feeds the KPI cards (profit factor, PnL curve, …). `raw_json` keeps
-- the full upstream object so new cards can read fields we don't column-map yet.
CREATE TABLE IF NOT EXISTS tt_trades (
    tt_id       TEXT PRIMARY KEY NOT NULL,
    symbol      TEXT,
    side        TEXT,
    quantity    REAL,
    entry_price REAL,
    exit_price  REAL,
    pnl         REAL,
    pnl_percent REAL,
    entry_date  TEXT,
    exit_date   TEXT,
    commission  REAL,
    fees        REAL,
    status      TEXT,
    setup       TEXT,
    strategy    TEXT,
    broker      TEXT,
    tags_json   TEXT NOT NULL DEFAULT '[]',
    raw_json    TEXT NOT NULL DEFAULT '{}',
    synced_at   TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_tt_trades_exit ON tt_trades(exit_date);

-- ── Attention du marché (écran détaché) — see `crate::attention_map` ───────────
-- Rolling history: every symbol's dollars per 5-minute slot for one extended
-- session (`part` = ah | pm) of one ET day, zstd-compressed. It is what the
-- 60-session same-hour medians are computed from, and it grows by itself — the
-- live frame is archived when it completes. `symbols` = 0 marks a day that turned
-- out to have no session, so it is never asked for again.
CREATE TABLE IF NOT EXISTS attention_archive (
    day        TEXT NOT NULL,
    part       TEXT NOT NULL,
    symbols    INTEGER NOT NULL,
    payload    BLOB NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (day, part)
);

-- The whole universe's slots for a frame (after-hours of the previous session +
-- premarket of `day`), so a restart or a second replay of the day downloads only
-- what is missing. Short-lived: only the most recent frames are kept.
CREATE TABLE IF NOT EXISTS attention_frames (
    day        TEXT PRIMARY KEY NOT NULL,
    prev_day   TEXT NOT NULL,
    payload    BLOB NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
"#;

/// Idempotent column additions for DBs created before the column existed.
/// `ALTER TABLE … ADD COLUMN` errors if the column is already present, so the
/// error is swallowed — the column simply already exists. Statements on personal
/// tables carry the `user.` prefix; they also bring an older data pack forward.
const ALTERS: &[&str] = &[
    "ALTER TABLE user.price_alarms ADD COLUMN triggered_at TEXT",
    // Panic Mean Reversion was reworked into a two-list watchlist (`panic_watchlist`);
    // the old composite-scoring table is obsolete. Dropped so a stale schema can't
    // linger. (Not strictly an ALTER, but the same idempotent-migration channel.)
    "DROP TABLE IF EXISTS mean_reversion_scores",
    // Panic Mean Reversion again: the BB-area / SMA20-contact rankings measured
    // DISTANCE, never speed, so a slow grind scored like a crash. Replaced by
    // `panic_watchlist_v2` (sigma-spike × ATR-expansion, keyed by day). The old
    // table is dropped here — its CREATE + CREATE INDEX were removed from
    // MIGRATIONS in the same change, which is mandatory: MIGRATIONS runs FIRST and
    // its errors abort startup, so a lingering `CREATE INDEX … ON panic_watchlist`
    // would fail on the second launch once this DROP has run.
    "DROP TABLE IF EXISTS panic_watchlist",
    // The day marker moved into the `panic_watchlist_build` ledger.
    "DELETE FROM app_config WHERE key='panic_watchlist_date'",
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
    // Authoritative security classification from Massive reference tickers. Used by
    // Panic Mean Reversion to exclude ETFs/ETNs/funds/preferreds without guessing
    // from listing venue or issuer name.
    "ALTER TABLE universe_assets ADD COLUMN security_type TEXT",
    "ALTER TABLE panic_watchlist_v2 ADD COLUMN setup_kind TEXT NOT NULL DEFAULT 'multiday'",
    "ALTER TABLE panic_watchlist_v2 ADD COLUMN move_pct REAL NOT NULL DEFAULT 0",
    "ALTER TABLE panic_watchlist_v2 ADD COLUMN stability_score REAL NOT NULL DEFAULT 0",
    "ALTER TABLE panic_watchlist_v2 ADD COLUMN volume_expansion REAL",
    "ALTER TABLE panic_watchlist_v2 ADD COLUMN directional_efficiency REAL",
    "ALTER TABLE panic_watchlist_v2 ADD COLUMN consecutive_bars INTEGER",
    "ALTER TABLE user.panic_llm_scores ADD COLUMN ranking_score REAL",
    "ALTER TABLE user.panic_llm_scores ADD COLUMN trade_direction TEXT",
    "ALTER TABLE user.panic_llm_scores ADD COLUMN timing_state TEXT",
    "ALTER TABLE user.panic_llm_scores ADD COLUMN fundamental_veto INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE user.panic_llm_scores ADD COLUMN veto_reason TEXT",
    "ALTER TABLE user.panic_llm_scores ADD COLUMN eligibility TEXT",
    "ALTER TABLE user.panic_llm_scores ADD COLUMN catalyst_direction TEXT",
    "CREATE INDEX IF NOT EXISTS user.idx_panic_llm_rank ON panic_llm_scores(day, simulated, fundamental_veto, ranking_score DESC)",
    // Editable drawings: per-timeframe scope + style columns (TradingView-like).
    "ALTER TABLE user.chart_drawings ADD COLUMN scope TEXT NOT NULL DEFAULT 'intraday'",
    "ALTER TABLE user.chart_drawings ADD COLUMN color TEXT",
    "ALTER TABLE user.chart_drawings ADD COLUMN opacity REAL",
    "ALTER TABLE user.chart_drawings ADD COLUMN width REAL",
    "ALTER TABLE user.chart_drawings ADD COLUMN line_style TEXT",
    "ALTER TABLE user.chart_drawings ADD COLUMN font_size REAL",
    // Launch-time TP, added after the original-SL column (green counterpart of the
    // original-SL chart segment).
    "ALTER TABLE user.trade_levels ADD COLUMN original_tp REAL",
    // Micro Pullback v2 anti-noise evidence flag was added after the first local
    // telemetry draft; keep databases opened during that draft writable.
    "ALTER TABLE micro_attention_events ADD COLUMN evidence_ok INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE micro_attention_snapshots ADD COLUMN evidence_ok INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE micro_attention_events ADD COLUMN tempo_rvol REAL",
    "ALTER TABLE micro_attention_events ADD COLUMN baseline_windows INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE micro_attention_snapshots ADD COLUMN tempo_rvol REAL",
    "ALTER TABLE micro_attention_snapshots ADD COLUMN baseline_windows INTEGER NOT NULL DEFAULT 0",
    // Market-time stamp of an A+ verdict, so the overlay can tell a score that
    // already saw the latest headline from one that predates it.
    "ALTER TABLE user.micro_quality_scores ADD COLUMN scored_at TEXT",
    // The three thirds of the score, stored separately: the pane shows them side by
    // side, and a single collapsed total cannot be re-split after the fact.
    "ALTER TABLE user.micro_quality_scores ADD COLUMN rvol_score REAL",
    "ALTER TABLE user.micro_quality_scores ADD COLUMN attention_score REAL",
    "ALTER TABLE user.micro_quality_scores ADD COLUMN news_score REAL",
    "CREATE INDEX IF NOT EXISTS user.idx_executions_trade ON executions(trade_id)",
    // Chart provenance for an execution marker: the market instant the fill price
    // belongs to (≠ `filled_at` on a 15-minute delayed plan), and the sizing button
    // that launched an entry (25 / 50 / 100). Both NULL on rows written earlier.
    "ALTER TABLE user.executions ADD COLUMN data_at TEXT",
    "ALTER TABLE user.executions ADD COLUMN size_pct INTEGER",
    "CREATE INDEX IF NOT EXISTS user.idx_screenshot_files_trade ON screenshot_files(trade_id)",
];

/// Attach an in-memory personal database when the connection has none. A real
/// connection comes from `local_db::open` with `user.db` attached; a bare one (the
/// in-memory databases of the tests) gets the same shape, so one schema serves both.
fn ensure_user_attached(conn: &Connection) -> Result<()> {
    let attached = conn
        .prepare("PRAGMA database_list")?
        .query_map([], |r| r.get::<_, String>(1))?
        .filter_map(|r| r.ok())
        .any(|name| name == USER_SCHEMA_NAME);
    if !attached {
        conn.execute_batch("ATTACH DATABASE ':memory:' AS user")?;
    }
    Ok(())
}

/// The personal schema alone, on a connection whose main database must not be
/// written (the read-only harnesses — `local_db::open_harness`).
#[cfg(test)]
pub(crate) fn create_user_schema(conn: &Connection) -> Result<()> {
    ensure_user_attached(conn)?;
    conn.execute_batch(USER_SCHEMA)
}

pub fn migrate(conn: &Connection) -> Result<()> {
    ensure_user_attached(conn)?;
    conn.execute_batch(PRAGMAS)?;
    conn.execute_batch(USER_SCHEMA)?;
    // A database from before the separation still holds the personal tables in
    // `main`, where they would shadow the ones just created. Move them across
    // before anything reads a row.
    super::split::run(conn)?;
    conn.execute_batch(CACHE_SCHEMA)?;
    for stmt in ALTERS {
        let _ = conn.execute(stmt, []); // ignore "duplicate column name"
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn tables(conn: &Connection, schema: &str) -> BTreeSet<String> {
        conn.prepare(&format!("SELECT name FROM {schema}.sqlite_master WHERE type = 'table'"))
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .filter(|n| !n.starts_with("sqlite_"))
            .collect()
    }

    /// THE guard on the split. `USER_TABLES` is what the separation, the overview and
    /// the data pack all trust; a personal table created under `user.` and forgotten
    /// in the list would silently stay out of a pack.
    #[test]
    fn the_personal_schema_is_exactly_the_declared_list() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        let declared: BTreeSet<String> = USER_TABLES.iter().map(|s| s.to_string()).collect();
        assert_eq!(tables(&conn, "user"), declared);
    }

    /// A name present in both files would resolve to `main` and hide the personal one.
    #[test]
    fn no_table_lives_in_both_databases() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        let both: Vec<String> =
            tables(&conn, "main").intersection(&tables(&conn, "user")).cloned().collect();
        assert!(both.is_empty(), "shadowed personal tables: {both:?}");
    }

    #[test]
    fn migrating_twice_is_harmless() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        conn.execute("INSERT INTO executions (fill_id, trade_id, symbol, quantity, fill_price, filled_at)
                      VALUES ('f1', 't1', 'AAPL', 10, 1.0, '2026-10-12T13:30:00Z')", [])
            .unwrap();
        migrate(&conn).unwrap();
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM user.executions", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }
}
