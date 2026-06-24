import { invoke } from "@tauri-apps/api/core";
import type {
  AlarmView,
  AlertEnrichment,
  AlertSignal,
  AppConfig,
  AttentionEntry,
  AppStatus,
  Bar,
  BugReport,
  CardInfo,
  CardNews,
  ChartDrawing,
  DailyBackground,
  DashboardTrade,
  FeedDiagnostics,
  FlatFileDay,
  FlatFilesKind,
  FlatFilesStatus,
  Fill,
  InternalOrder,
  JournalEntry,
  LatencyStatus,
  LocalLogEntry,
  MarketSnapshot,
  Mood,
  NewsDiagnostics,
  Position,
  PrevDayLevels,
  PriceAlarm,
  ReplayStatus,
  ScreenerMatch,
  SecretsStatus,
  SecretsUpdate,
  SplitMarker,
  NewsMarker,
  StartupState,
  Strategy,
  StrategyCard,
  StreamableSymbol,
  SyncQueueStatus,
  TickerTableRow,
  Timeframe,
  TradeExecutions,
  TradeLifecycle,
  ZoneTradeContext,
} from "@/types";

export const api = {
  // Status
  getAppStatus: () => invoke<AppStatus>("get_app_status"),

  // Dashboard (moodboard)
  syncTradetallyTrades:  () => invoke<number>("sync_tradetally_trades"),
  getDashboardTrades:    () => invoke<DashboardTrade[]>("get_dashboard_trades"),
  saveDiaryEntry:        (title: string, content: string) =>
    invoke<void>("save_diary_entry", { title, content }),
  getDailyBackground:    () => invoke<DailyBackground>("get_daily_background"),
  openBackgroundsFolder: () => invoke<void>("open_backgrounds_folder"),
  getMood:               () => invoke<Mood>("get_mood"),
  openMoodTarget:        (target: "images" | "short" | "long") =>
    invoke<void>("open_mood_target", { target }),
  // Bundled default dashboard layout (seeds a fresh user's board).
  getDefaultDashboard:   () => invoke<string | null>("get_default_dashboard"),
  // Maintainer: save the current layout to disk so it can be bundled as the default.
  exportDashboardDefault: (layout_json: string) =>
    invoke<string>("export_dashboard_default", { layout_json }),

  // Embedded TradeTally webview (native child webview positioned over its tab)
  tradetallySetBounds: (x: number, y: number, width: number, height: number) =>
    invoke<void>("tradetally_set_bounds", { x, y, width, height }),
  tradetallyHide:      () => invoke<void>("tradetally_hide"),

  // Config
  getLocalConfig:    () => invoke<AppConfig>("get_local_config"),
  updateLocalConfig: (config: AppConfig) =>
    invoke<void>("update_local_config", { config }),

  // Secrets — status (booleans) + write-only update (values never read back)
  getSecretsStatus: () => invoke<SecretsStatus>("get_secrets_status"),
  updateSecrets: (updates: SecretsUpdate) =>
    invoke<SecretsStatus>("update_secrets", { updates }),

  // Journal tags (user-defined) + TradeTally queue, retry
  getJournalTags:          () => invoke<string[]>("get_journal_tags"),
  getSyncQueueStatus:      () => invoke<SyncQueueStatus>("get_sync_queue_status"),
  retryTradeTallyEvent:    (event_id: string) =>
    invoke<void>("retry_tradetally_event", { event_id }),
  retryAllTradeTallyEvents: () => invoke<void>("retry_all_tradetally_events"),

  // Journal
  saveJournalEntry: (trade_id: string, symbol: string, notes: string, confidence: number | null, tags: string[]) =>
    invoke<void>("save_journal_entry", { trade_id, symbol, notes, confidence, tags }),
  getJournalEntry: (trade_id: string) =>
    invoke<JournalEntry | null>("get_journal_entry", { trade_id }),

  // Screenshot
  saveScreenshotLocal: (zone_id: string, trade_id: string | null, image_base64: string, filename: string) =>
    invoke<string>("save_screenshot_local", { zone_id, trade_id, image_base64, filename }),

  // Logs
  getLocalLogs: (limit?: number) =>
    invoke<LocalLogEntry[]>("get_local_logs", { limit }),

  // Bug reports (persisted)
  getBugReports:   () => invoke<BugReport[]>("get_bug_reports"),
  addBugReport:    (id: string, text: string, priority: number) =>
    invoke<BugReport[]>("add_bug_report", { id, text, priority }),
  deleteBugReport: (id: string) =>
    invoke<BugReport[]>("delete_bug_report", { id }),
  clearBugReports: () => invoke<BugReport[]>("clear_bug_reports"),

  // Price alarms (persisted; triggering not wired yet)
  createAlarm: (id: string, symbol: string, strategy_id: string | null, price: number) =>
    invoke<PriceAlarm>("create_alarm", { id, symbol, strategy_id, price }),
  getAlarmsForSymbol: (symbol: string) =>
    invoke<PriceAlarm[]>("get_alarms_for_symbol", { symbol }),
  getAllAlarms: () => invoke<AlarmView[]>("get_all_alarms"),
  deleteAlarm:  (id: string) => invoke<void>("delete_alarm", { id }),

  // Dev / mock alerts
  getMockAlerts: () => invoke<AlertSignal[]>("get_mock_alerts"),

  // Startup pipeline
  runStartupPipeline:    () => invoke<void>("run_startup_pipeline"),
  getStartupStatus:      () => invoke<StartupState>("get_startup_status"),
  getStreamableUniverse: () => invoke<StreamableSymbol[]>("get_streamable_universe"),

  // Live market feed
  startMockMarketFeed: () => invoke<void>("start_mock_market_feed"),
  stopMockMarketFeed:  () => invoke<void>("stop_mock_market_feed"),
  startLiveFeed:       () => invoke<void>("start_live_feed"),
  stopLiveFeed:        () => invoke<void>("stop_live_feed"),
  startNewsFeed:       () => invoke<void>("start_news_feed"),
  stopNewsFeed:        () => invoke<void>("stop_news_feed"),
  setFocusSymbols:     (symbols: string[]) =>
    invoke<void>("set_focus_symbols", { symbols }),
  getMarketSnapshot:   () => invoke<MarketSnapshot>("get_market_snapshot"),
  getTickerBars: (symbol: string, timeframe: Timeframe) =>
    invoke<Bar[]>("get_ticker_bars", { symbol, timeframe }),
  // Unified bar loader: refreshes (gap-fills + today's session) from Alpaca and
  // merges into RAM, for every pane / strategy / timeframe.
  loadChartBars: (symbol: string, timeframe: Timeframe) =>
    invoke<Bar[]>("load_chart_bars", { symbol, timeframe }),
  // Lazily back-fill older history (batch) as the user scrolls into the past.
  loadOlderBars: (symbol: string, timeframe: Timeframe, before: string, limit: number) =>
    invoke<Bar[]>("load_older_bars", { symbol, timeframe, before, limit }),
  // Historical split-day markers (Alpaca corporate-actions, last 2y) for any
  // daily chart — red dots on the split ex-dates.
  getSplitMarkers: (symbol: string) =>
    invoke<SplitMarker[]>("get_split_markers", { symbol }),
  // Single-ticker news timestamps over a wide window — a small pastille per bar
  // with news (intraday + daily). The frontend snaps each to the nearest bar.
  getNewsMarkers: (symbol: string) =>
    invoke<NewsMarker[]>("get_news_markers", { symbol }),
  // Previous trading day's reference levels (PDC/PDH/PDL) relative to today.
  getPreviousDayLevels: (symbol: string) =>
    invoke<PrevDayLevels | null>("get_previous_day_levels", { symbol }),
  getCardInfo: (symbol: string) =>
    invoke<CardInfo>("get_card_info", { symbol }),
  // Most recent single-ticker headlines for a displayed ticker (Alpaca news REST,
  // headlines only, multi-ticker news dropped). Up to 4, newest first.
  getTickerNews: (symbol: string) =>
    invoke<CardNews[]>("get_ticker_news", { symbol }),
  // Bounded extract of the tickers data table (universe DB + all enrichments +
  // news/filings counts). Empty query → most recently collected rows; otherwise →
  // tickers matching the query (symbol prefix / name contains). Capped server-side.
  getTickersTable: (query: string, limit: number) =>
    invoke<TickerTableRow[]>("get_tickers_table", { query, limit }),
  getLatencyStatus: () => invoke<LatencyStatus>("get_latency_status"),
  getFeedDiagnostics: () => invoke<FeedDiagnostics>("get_feed_diagnostics"),
  getNewsDiagnostics: () => invoke<NewsDiagnostics>("get_news_diagnostics"),

  // Scanner
  getStrategies:    () => invoke<Strategy[]>("get_strategies"),
  setStrategyEnabled: (strategy_id: string, enabled: boolean) =>
    invoke<void>("set_strategy_enabled", { strategy_id, enabled }),
  setStrategyRisk: (strategy_id: string, max_risk_dollars: number) =>
    invoke<void>("set_strategy_risk", { strategy_id, max_risk_dollars }),
  getStrategyCards: () => invoke<Record<string, StrategyCard>>("get_strategy_cards"),
  startAlertEnrichment: (symbol: string, strategy_id: string) =>
    invoke<void>("start_alert_enrichment", { symbol, strategy_id }),
  runAlertLlm: (symbol: string, strategy_id: string) =>
    invoke<void>("run_alert_llm", { symbol, strategy_id }),
  getAlertEnrichment: (symbol: string) =>
    invoke<AlertEnrichment | null>("get_alert_enrichment", { symbol }),
  getActiveAlerts: () => invoke<AlertSignal[]>("get_active_alerts"),
  getAlertHistory: () => invoke<AlertSignal[]>("get_alert_history"),
  getScreenerMatches: () => invoke<ScreenerMatch[]>("get_screener_matches"),
  // Market Attention top list (direction-agnostic, top 10, refreshed 1×/min
  // 09:30–12:30 ET). Debug/inspection only — the primary consumer is the backend
  // Perfect Pullback engine.
  getMarketAttention: () => invoke<AttentionEntry[]>("get_market_attention"),
  // Pre-open screener dismissals, persisted per trading day.
  dismissScreener: (symbol: string) => invoke<void>("dismiss_screener", { symbol }),
  getScreenerDismissals: () => invoke<string[]>("get_screener_dismissals"),
  startScanner:    () => invoke<void>("start_scanner"),
  stopScanner:     () => invoke<void>("stop_scanner"),

  // Chart / zone trade context
  getZoneTradeContext: (zone_id: string, symbol: string) =>
    invoke<ZoneTradeContext | null>("get_zone_trade_context", { zone_id, symbol }),
  createOrGetTradeIdForZone: (zone_id: string, symbol: string, strategy_id: string) =>
    invoke<string>("create_or_get_trade_id_for_zone", { zone_id, symbol, strategy_id }),
  updateZoneSl: (zone_id: string, symbol: string, strategy_id: string, price: number | null) =>
    invoke<ZoneTradeContext>("update_zone_sl", { zone_id, symbol, strategy_id, price }),
  updateZoneTp: (zone_id: string, symbol: string, strategy_id: string, price: number | null) =>
    invoke<ZoneTradeContext>("update_zone_tp", { zone_id, symbol, strategy_id, price }),
  clearZoneContext: (zone_id: string) =>
    invoke<void>("clear_zone_context", { zone_id }),

  // Internal trading engine
  createInternalOrderPercent: (zone_id: string, percent: number) =>
    invoke<InternalOrder>("create_internal_order_percent", { zone_id, percent }),
  createInternalMarketOrderPercent: (zone_id: string, percent: number) =>
    invoke<Fill>("create_internal_market_order_percent", { zone_id, percent }),
  cancelInternalOrder: (order_id: string) =>
    invoke<void>("cancel_internal_order", { order_id }),
  closeInternalPosition: (symbol: string, zone_id: string) =>
    invoke<Fill>("close_internal_position", { symbol, zone_id }),
  getInternalPositions: () =>
    invoke<Position[]>("get_internal_positions"),
  getInternalOrders: () =>
    invoke<InternalOrder[]>("get_internal_orders"),
  getTradeLifecycle: (trade_id: string) =>
    invoke<TradeLifecycle | null>("get_trade_lifecycle", { trade_id }),

  // Persisted trade executions for a ticker → chart triangles + P&L line.
  getExecutionsForSymbol: (symbol: string) =>
    invoke<TradeExecutions[]>("get_executions_for_symbol", { symbol }),

  // User chart drawings (trend lines / text), persisted per ticker.
  createDrawing: (drawing: ChartDrawing) =>
    invoke<void>("create_drawing", { drawing }),
  getDrawingsForSymbol: (symbol: string) =>
    invoke<ChartDrawing[]>("get_drawings_for_symbol", { symbol }),
  updateDrawing: (drawing: ChartDrawing) =>
    invoke<void>("update_drawing", { drawing }),
  deleteDrawing: (id: string) =>
    invoke<void>("delete_drawing", { id }),
  updateAlarmPrice: (id: string, price: number) =>
    invoke<void>("update_alarm_price", { id, price }),

  // Market Replay
  replayStart: (day: string, start_hm: string) =>
    invoke<void>("replay_start", { day, start_hm }),
  replayStop:       () => invoke<void>("replay_stop"),
  replaySetPlaying: (playing: boolean) =>
    invoke<void>("replay_set_playing", { playing }),
  replaySetSpeed:   (speed: number) =>
    invoke<void>("replay_set_speed", { speed }),
  replaySeekRelative: (delta_secs: number) =>
    invoke<void>("replay_seek_relative", { delta_secs }),
  replaySeekClock: (hm: string) =>
    invoke<void>("replay_seek_clock", { hm }),
  replayNextAlert: () => invoke<void>("replay_next_alert"),
  replayNextDay:   () => invoke<void>("replay_next_day"),
  getReplayStatus: () => invoke<ReplayStatus>("get_replay_status"),

  // Flat files (offline market-data download for Market Replay)
  flatFilesDownload: (kind: FlatFilesKind, start_day: string, end_day: string) =>
    invoke<void>("flat_files_download", { kind, start_day, end_day }),
  flatFilesCancel:      () => invoke<void>("flat_files_cancel"),
  getFlatFilesStatus:   () => invoke<FlatFilesStatus>("get_flat_files_status"),
  getFlatFilesCalendar: (kind: FlatFilesKind) =>
    invoke<FlatFileDay[]>("get_flat_files_calendar", { kind }),
  openFlatFilesFolder:  (kind: FlatFilesKind) =>
    invoke<void>("open_flat_files_folder", { kind }),
};
