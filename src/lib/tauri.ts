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
  HodDriveOverlay,
  ChartDrawing,
  DailyBackground,
  DashboardTrade,
  DiaryEntry,
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
  ReplaySource,
  ReplayStatus,
  MicTestResult,
  ScreenerMatch,
  SecretsStatus,
  SecretsUpdate,
  SplitMarker,
  SttJobKind,
  SttStatus,
  NewsMarker,
  StartupState,
  Strategy,
  StrategyCard,
  StrategyRiskConfig,
  MicroPullbackOptions,
  ApexOptions,
  ApexStatus,
  DipOptions,
  DipStatus,
  DislocationOptions,
  BacksideOptions,
  MicroAttentionEntry,
  StreamableSymbol,
  SyncQueueStatus,
  TickerTableRow,
  TodoTrade,
  TradeDbRow,
  Timeframe,
  TradeExecutions,
  TradeLifecycle,
  TradeLink,
  ZoneTradeContext,
  BrokerAccount,
  BrokerMirror,
  BrokerSelection,
  MassiveTierStatus,
  Shortability,
  ShortCostZone,
  PanicRunState,
  PanicLlmScore,
  MicroQualityBoard,
  MicroPullbackQualityRow,
  TradeSyncReport,
} from "@/types";

export const api = {
  // Status
  getAppStatus: () => invoke<AppStatus>("get_app_status"),

  // Dashboard (moodboard)
  syncTradetallyTrades:  () => invoke<TradeSyncReport>("sync_tradetally_trades"),
  getDashboardTrades:    () => invoke<DashboardTrade[]>("get_dashboard_trades"),
  saveDiaryEntry:        (title: string, content: string) =>
    invoke<void>("save_diary_entry", { title, content }),
  // TradeTally's own entry for the day (defaults to today, ET) — what the source
  // of truth holds, as opposed to the local draft.
  getDiaryEntry:         (entry_date?: string) =>
    invoke<DiaryEntry | null>("get_diary_entry", { entry_date: entry_date ?? null }),
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

  // Embedded TradeTally child webview (created/destroyed by the TradeTally tab).
  tradetallySetBounds: (x: number, y: number, width: number, height: number) =>
    invoke<void>("tradetally_set_bounds", { x, y, width, height }),
  tradetallyClose:     () => invoke<void>("tradetally_close"),
  tradetallyReload:    () => invoke<void>("tradetally_reload"),
  tradetallyClearData: () => invoke<void>("tradetally_clear_data"),

  // Flash alert overlay — built on demand from Settings (never at startup).
  setFlashOverlay: (enabled: boolean) =>
    invoke<void>("set_flash_overlay", { enabled }),

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

  // Which trade the journal / screenshot actually belong to for a ticker — see
  // `TradeLink`. Resolved backend-side so every caller agrees.
  getTradeLink: (zone_id: string, symbol: string) =>
    invoke<TradeLink>("get_trade_link", { zone_id, symbol }),

  // Journal
  saveJournalEntry: (trade_id: string, symbol: string, notes: string, confidence: number | null, tags: string[]) =>
    invoke<void>("save_journal_entry", { trade_id, symbol, notes, confidence, tags }),
  getJournalEntry: (trade_id: string) =>
    invoke<JournalEntry | null>("get_journal_entry", { trade_id }),

  // Screenshot
  saveScreenshotLocal: (zone_id: string, trade_id: string | null, image_base64: string, filename: string) =>
    invoke<string>("save_screenshot_local", { zone_id, trade_id, image_base64, filename }),

  // Todo trades (missing screenshot / journal)
  getTodoTrades: () => invoke<TodoTrade[]>("get_todo_trades"),

  // All trades DB view
  getAllTradesDb:  () => invoke<TradeDbRow[]>("get_all_trades_db"),
  getTradeDays:   () => invoke<string[]>("get_trade_days"),
  deleteTradeDb:  (trade_id: string) => invoke<void>("delete_trade_db", { trade_id }),

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

  // Startup pipeline
  runStartupPipeline:    () => invoke<void>("run_startup_pipeline"),
  getStartupStatus:      () => invoke<StartupState>("get_startup_status"),
  getStreamableUniverse: () => invoke<StreamableSymbol[]>("get_streamable_universe"),

  // Live market feed
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
  // Premarket (04:00-09:30 ET) daily candles from `from_date` to today — the ghost
  // series the daily pane draws behind its regular-session candles. Neither
  // provider's daily bar contains this; it is folded from 15-min bars and cached,
  // so widening the window only fetches what is genuinely missing.
  loadPremarketDaily: (symbol: string, from_date: string) =>
    invoke<Bar[]>("load_premarket_daily", { symbol, from_date }),
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
  // HOD Drive on-chart overlay: the five KPIs + HOD/LOD levels + green-series bar
  // times for the chart points/crosses. Recomputed live for the displayed ticker.
  getHodDriveOverlay: (symbol: string) =>
    invoke<HodDriveOverlay>("get_hod_drive_overlay", { symbol }),
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
  setStrategyRisk: (strategy_id: string, risk: StrategyRiskConfig) =>
    invoke<void>("set_strategy_risk", { strategy_id, risk }),
  getStrategyCards: () => invoke<Record<string, StrategyCard>>("get_strategy_cards"),
  testNtfy: () => invoke<void>("test_ntfy"),
  getMicroPullbackOptions: () =>
    invoke<MicroPullbackOptions>("get_micro_pullback_options"),
  setMicroPullbackOptions: (options: MicroPullbackOptions) =>
    invoke<void>("set_micro_pullback_options", { options }),
  // Apex (Micro Pullback): options, live status, and the arm/disarm hotkey.
  getApexOptions: () => invoke<ApexOptions>("get_apex_options"),
  setApexOptions: (options: ApexOptions) =>
    invoke<void>("set_apex_options", { options }),
  getApexStatus: () => invoke<ApexStatus>("get_apex_status"),
  apexToggle: (zone_id: string, symbol: string) =>
    invoke<ApexStatus>("apex_toggle", { zone_id, symbol }),

  // Dip (Micro Pullback): options, live status, and the band hotkeys. `dip_place`
  // carries the price under the cursor — the backend decides whether that lays the
  // band, moves it, or (pointing back inside it) takes it away.
  getDipOptions: () => invoke<DipOptions>("get_dip_options"),
  setDipOptions: (options: DipOptions) =>
    invoke<void>("set_dip_options", { options }),
  getDipStatus: () => invoke<DipStatus>("get_dip_status"),
  dipPlace: (zone_id: string, symbol: string, price: number) =>
    invoke<DipStatus>("dip_place", { zone_id, symbol, price }),
  dipResize: (steps: number) => invoke<DipStatus>("dip_resize", { steps }),
  getDislocationAttention: () =>
    invoke<MicroAttentionEntry[]>("get_dislocation_attention"),
  getDislocationOptions: () =>
    invoke<DislocationOptions>("get_dislocation_options"),
  setDislocationOptions: (options: DislocationOptions) =>
    invoke<void>("set_dislocation_options", { options }),
  getBacksideOptions: () => invoke<BacksideOptions>("get_backside_options"),
  setBacksideOptions: (options: BacksideOptions) =>
    invoke<void>("set_backside_options", { options }),
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
  // Shares outstanding from SEC EDGAR (free, keyless) → market cap → the size
  // buckets of the Panic screener. Quarterly refresh is enough.
  refreshSecShares:   () => invoke<number>("refresh_sec_shares"),
  secSharesCoverage:  () => invoke<number>("sec_shares_coverage"),
  // Pre-open screener pins. NOT day-scoped: a pin keeps the ticker on the list on
  // the following days until removed. Stored separately for live vs Market Replay.
  pinScreener:   (symbol: string) => invoke<void>("pin_screener", { symbol }),
  unpinScreener: (symbol: string) => invoke<void>("unpin_screener", { symbol }),
  getScreenerPins: () => invoke<string[]>("get_screener_pins"),
  startScanner:    () => invoke<void>("start_scanner"),
  stopScanner:     () => invoke<void>("stop_scanner"),

  // Panic Score — LLM ranking of the VISIBLE pre-open watchlist. Both lists are passed
  // in deliberately: dismissals exist only in the UI, so the backend cannot derive
  // either one. `symbols` is what to score; `dismissed` is what the run's catch-up wave
  // must NOT score when the watchlist is rebuilt mid-run.
  runPanicLlmAnalysis: (symbols: string[], dismissed: string[]) =>
    invoke<void>("run_panic_llm_analysis", { symbols, dismissed }),
  getPanicLlmRun:      () => invoke<PanicRunState>("get_panic_llm_run"),
  getPanicLlmScores:   () => invoke<PanicLlmScore[]>("get_panic_llm_scores"),
  // Micro Pullback A+: the day's verdicts AND what is still in flight, in one call —
  // polled separately the two would disagree for a tick and the pane would flicker.
  getDislocationScores: () => invoke<MicroQualityBoard>("get_dislocation_scores"),
  // Micro Pullback's own (lighter, News-only) catalyst reads for the day.
  getMicroPullbackScores: () =>
    invoke<MicroPullbackQualityRow[]>("get_micro_pullback_scores"),
  runMicroPullbackNewsScore: (symbol: string) =>
    invoke<void>("run_micro_pullback_news_score", { symbol }),
  hasWebSearchProvider: () => invoke<boolean>("has_web_search_provider"),

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

  // Order entry. Routed by the ACTIVE broker: internal simulator, or straight to
  // Alpaca when Alpaca is selected. These return nothing on purpose — with a real
  // venue there is no result to return synchronously (a market order is filled by
  // the venue, not here), so the outcome arrives through the positions/orders
  // queries. A rejection surfaces as a rejected promise → notification.
  createInternalOrderPercent: (zone_id: string, percent: number) =>
    invoke<void>("create_internal_order_percent", { zone_id, percent }),
  createInternalLimitOrderPercent: (zone_id: string, percent: number, limit_price: number) =>
    invoke<void>("create_internal_limit_order_percent", { zone_id, percent, limit_price }),
  createInternalMarketOrderPercent: (zone_id: string, percent: number) =>
    invoke<void>("create_internal_market_order_percent", { zone_id, percent }),
  cancelInternalOrder: (order_id: string) =>
    invoke<void>("cancel_internal_order", { order_id }),
  closeInternalPosition: (symbol: string, zone_id: string) =>
    invoke<void>("close_internal_position", { symbol, zone_id }),
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
  replayStart: (day: string, start_hm: string, source: ReplaySource = "api") =>
    invoke<void>("replay_start", { day, start_hm, source }),
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
  replayNextBar:   () => invoke<void>("replay_next_bar"),
  replayNextDay:   () => invoke<void>("replay_next_day"),
  getReplayStatus: () => invoke<ReplayStatus>("get_replay_status"),

  // Flat files (offline market-data download for Market Replay)
  flatFilesDownload: (kind: FlatFilesKind, start_day: string, end_day: string) =>
    invoke<void>("flat_files_download", { kind, start_day, end_day }),
  flatFilesCancel:      () => invoke<void>("flat_files_cancel"),
  // Broker — account selection + the venue's own view of orders/positions.
  getScreenerRatings: () => invoke<Record<string, number>>("get_screener_ratings"),
  setScreenerRating:  (symbol: string, rating: number) =>
    invoke<void>("set_screener_rating", { symbol, rating }),

  getBrokerSelection: () => invoke<BrokerSelection>("get_broker_selection"),
  setBrokerSelection: (broker_id: string, environment: string) =>
    invoke<BrokerSelection>("set_broker_selection", { broker_id, environment }),
  getBrokerMirror:    () => invoke<BrokerMirror>("get_broker_mirror"),
  getMassiveTierStatus:   () => invoke<MassiveTierStatus>("get_massive_tier_status"),
  setMassiveTierOverride: (tier: "unlimited" | "basic" | null) =>
    invoke<MassiveTierStatus>("set_massive_tier_override", { tier }),
  getBrokerAccount:   () => invoke<BrokerAccount | null>("get_broker_account"),
  brokerCancelOrder:  (order_id: string) =>
    invoke<void>("broker_cancel_order", { order_id }),
  brokerClosePosition: (symbol: string) =>
    invoke<void>("broker_close_position", { symbol }),
  getShortability:    (symbol: string) =>
    invoke<Shortability>("get_shortability", { symbol }),
  getNonShortableSymbols: () => invoke<string[]>("get_non_shortable_symbols"),
  prepareShortLocate: (zone_id: string) =>
    invoke<ShortCostZone>("prepare_short_locate", { zone_id }),
  getShortCostZone: (zone_id: string) =>
    invoke<ShortCostZone | null>("get_short_cost_zone", { zone_id }),

  getFlatFilesStatus:   () => invoke<FlatFilesStatus>("get_flat_files_status"),
  getFlatFilesCalendar: (kind: FlatFilesKind) =>
    invoke<FlatFileDay[]>("get_flat_files_calendar", { kind }),
  openFlatFilesFolder:  (kind: FlatFilesKind) =>
    invoke<void>("open_flat_files_folder", { kind }),

  // Speech-to-Text dictée pipeline (offline whisper.cpp → trade notes / diary)
  sttStatus:           () => invoke<SttStatus>("stt_status"),
  sttDownloadModel:    () => invoke<void>("stt_download_model"),
  sttStartRecording:   (kind: SttJobKind, trade_id?: string | null, symbol?: string | null) =>
    invoke<void>("stt_start_recording", { kind, trade_id: trade_id ?? null, symbol: symbol ?? null }),
  sttStopRecording:    () => invoke<string>("stt_stop_recording"),
  sttCancelRecording:  () => invoke<void>("stt_cancel_recording"),
  sttCancelJob:        (id: string) => invoke<void>("stt_cancel_job", { id }),
  sttRetryJob:         (id: string) => invoke<void>("stt_retry_job", { id }),
  sttListInputDevices: () => invoke<string[]>("stt_list_input_devices"),
  sttTestMicrophone:   () => invoke<MicTestResult>("stt_test_microphone"),
};
