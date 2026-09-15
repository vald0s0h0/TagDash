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

-- Per-Alpaca-account runtime state. Open broker trades and extended-hours
-- protection plans must survive process restarts and account switching: positions
-- routinely stay open for several sessions/days.
CREATE TABLE IF NOT EXISTS broker_runtime_state (
    account_id      TEXT PRIMARY KEY NOT NULL,
    journal_json    TEXT,
    protection_json TEXT,
    updated_at      TEXT NOT NULL DEFAULT (datetime('now'))
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

-- Tickers the user PINNED in the pre-open screener. Unlike dismissals this is NOT
-- day-scoped: a pin persists across days until the user removes it. `simulated`
-- separates pins placed during a Market Replay from live ones so a backtest never
-- pollutes the real watchlist (and vice versa).
CREATE TABLE IF NOT EXISTS screener_pins (
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
CREATE TABLE IF NOT EXISTS screener_ratings (
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
CREATE TABLE IF NOT EXISTS llm_analysis (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    symbol      TEXT NOT NULL,
    strategy_id TEXT NOT NULL,
    context     TEXT,
    verdict     TEXT,
    created_at  TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_llm_analysis_symbol ON llm_analysis(symbol, created_at DESC);

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
CREATE TABLE IF NOT EXISTS panic_llm_scores (
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
CREATE INDEX IF NOT EXISTS idx_panic_llm_day
    ON panic_llm_scores(day, simulated, panic_score DESC);

-- Micro Pullback "Qualité A+" judgements. Keyed by (symbol, day, SESSION, simulated):
-- the engine's lock is per session, so one ticker can legitimately be judged twice in
-- a day — premarket and again at the open — on two different moves. Keying on the day
-- alone would let the second silently overwrite the first.
-- `pass1_score` (news scoring) and `pass2_score` (deep analysis) are kept side by side
-- so it stays answerable, weeks later, whether the deep pass ever changed an outcome.
CREATE TABLE IF NOT EXISTS micro_quality_scores (
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
CREATE INDEX IF NOT EXISTS idx_micro_quality_day
    ON micro_quality_scores(day, simulated, final_score DESC);

-- Micro Pullback's own news-scoring table. Deliberately separate from
-- micro_quality_scores (Dislocation's) rather than a shared table keyed by an added
-- strategy_id: the two engines already keep their runtime options under distinct
-- app_meta keys for the same reason (see micro_pullback::options header) — sharing a
-- table would mean rebuilding its primary key to avoid a symbol/day/session collision
-- between the two strategies, for no benefit over just not sharing it.
CREATE TABLE IF NOT EXISTS micro_pullback_quality_scores (
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
CREATE INDEX IF NOT EXISTS idx_micro_pullback_quality_day
    ON micro_pullback_quality_scores(day, simulated, score DESC);

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

-- Original (launch-time) stop loss / take profit per trade. Recorded ONCE on the
-- first fill and never overwritten, so they survive later SL/TP modifications —
-- they're the levels the journal/R:R is based on. Drive the thin "original SL"
-- (red) and "original TP" (green) segments drawn on every chart of the symbol
-- for the duration of the trade.
CREATE TABLE IF NOT EXISTS trade_levels (
    trade_id    TEXT PRIMARY KEY NOT NULL,
    symbol      TEXT NOT NULL,
    original_sl REAL,
    original_tp REAL,
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

-- Local copy of diary (journal) entries sent to TradeTally from the dashboard's
-- Journal card. Mirror only — the authoritative entry lives in TradeTally
-- (POST /api/diary, create-or-update per ET day). Kept for a future history view.
CREATE TABLE IF NOT EXISTS diary_entries_local (
    id         TEXT PRIMARY KEY NOT NULL,
    entry_date TEXT NOT NULL,
    title      TEXT NOT NULL DEFAULT '',
    content    TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
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
    "ALTER TABLE panic_llm_scores ADD COLUMN ranking_score REAL",
    "ALTER TABLE panic_llm_scores ADD COLUMN trade_direction TEXT",
    "ALTER TABLE panic_llm_scores ADD COLUMN timing_state TEXT",
    "ALTER TABLE panic_llm_scores ADD COLUMN fundamental_veto INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE panic_llm_scores ADD COLUMN veto_reason TEXT",
    "ALTER TABLE panic_llm_scores ADD COLUMN eligibility TEXT",
    "ALTER TABLE panic_llm_scores ADD COLUMN catalyst_direction TEXT",
    "CREATE INDEX IF NOT EXISTS idx_panic_llm_rank ON panic_llm_scores(day, simulated, fundamental_veto, ranking_score DESC)",
    // Editable drawings: per-timeframe scope + style columns (TradingView-like).
    "ALTER TABLE chart_drawings ADD COLUMN scope TEXT NOT NULL DEFAULT 'intraday'",
    "ALTER TABLE chart_drawings ADD COLUMN color TEXT",
    "ALTER TABLE chart_drawings ADD COLUMN opacity REAL",
    "ALTER TABLE chart_drawings ADD COLUMN width REAL",
    "ALTER TABLE chart_drawings ADD COLUMN line_style TEXT",
    "ALTER TABLE chart_drawings ADD COLUMN font_size REAL",
    // Launch-time TP, added after the original-SL column (green counterpart of the
    // original-SL chart segment).
    "ALTER TABLE trade_levels ADD COLUMN original_tp REAL",
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
    "ALTER TABLE micro_quality_scores ADD COLUMN scored_at TEXT",
    // The three thirds of the score, stored separately: the pane shows them side by
    // side, and a single collapsed total cannot be re-split after the fact.
    "ALTER TABLE micro_quality_scores ADD COLUMN rvol_score REAL",
    "ALTER TABLE micro_quality_scores ADD COLUMN attention_score REAL",
    "ALTER TABLE micro_quality_scores ADD COLUMN news_score REAL",
    "CREATE INDEX IF NOT EXISTS idx_executions_trade ON executions(trade_id)",
    "CREATE INDEX IF NOT EXISTS idx_screenshot_files_trade ON screenshot_files(trade_id)",
];

pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(MIGRATIONS)?;
    for stmt in ALTERS {
        let _ = conn.execute(stmt, []); // ignore "duplicate column name"
    }
    Ok(())
}
