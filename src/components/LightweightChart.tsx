import { useEffect, useRef, useState, useCallback } from "react";
import {
  createChart,
  CrosshairMode,
  LineStyle,
  TickMarkType,
  type IChartApi,
  type ISeriesApi,
  type IPriceLine,
  type UTCTimestamp,
  type CandlestickData,
  type LineData,
  type SeriesMarker,
  type Time,
} from "lightweight-charts";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { listen } from "@tauri-apps/api/event";
import { api } from "@/lib/tauri";
import type { Bar, PaneIndicator, Timeframe } from "@/types";
import type { DrawMode, ChartLine, ChartAnnotation, ChartAlarm } from "@/stores/chartStore";
import type { CrosshairSync } from "@/lib/crosshairSync";
import { nyTime, nyDateTime, nyDayMonth, nyMonth, nyYear, isExtendedHours } from "@/lib/nyTime";
import { ExecutionsPrimitive } from "@/charts/executionsPrimitive";
import {
  INDICATOR_COLORS, BOLLINGER_K, BOLLINGER_COLORS,
  indicatorId, computeEma, computeSma, computeBollinger,
} from "@/charts/indicators";
import {
  toUTC, TF_SECONDS, BACKFILL_THRESHOLD, BACKFILL_BATCH,
  slOpts, tpOpts, ENTRY_OPTIONS, BID_ASK_OPTIONS, ALARM_OPTIONS, PREV_DAY_OPTIONS,
} from "@/charts/chartOptions";

// ─── Props ────────────────────────────────────────────────────────────────────

interface Props {
  symbol:      string;
  timeframe:   Timeframe;
  drawMode:    DrawMode;
  slPrice:     number | null;
  tpPrice:     number | null;
  entryPrice:  number | null;
  bid:         number | null;
  ask:         number | null;
  ordersActive: boolean; // true when a position is open → SL/TP shown as orders
  lines:       ChartLine[];
  annotations: ChartAnnotation[];
  alarms:      ChartAlarm[];
  linePoint1:  { time: number; price: number } | null;
  /** Strategy-card indicators overlaid on this pane (VWAP / EMA / SMA). */
  indicators?: PaneIndicator[];
  /** Candle markers (e.g. red dots on split days), unix-seconds keyed. */
  markers?: { time: number; color: string; text?: string }[];
  /** Shared cross-pane crosshair sync group (same instrument). */
  crosshairSync?: CrosshairSync;
  /** Stable id of this pane within its zone, for the crosshair sync registry. */
  paneId?: string;
  onPriceClick:   (price: number, time: number, pixelX: number, pixelY: number) => void;
  onSlDragEnd:    (price: number) => void;
  onTpDragEnd:    (price: number) => void;
  /** Double-click shortcut: memorise the clicked price as a (provisional) SL
   *  without first arming the SL tool. Unlocks the 25/50/100 order buttons. */
  onSlDblClick:   (price: number) => void;
  /** Delete the user-placed SL / TP / alarm line from its on-chart ✕ button. */
  onDeleteSl?:    () => void;
  onDeleteTp?:    () => void;
  onDeleteAlarm?: (id: string) => void;
}

// ─── Component ────────────────────────────────────────────────────────────────

export function LightweightChart({
  symbol,
  timeframe,
  drawMode,
  slPrice,
  tpPrice,
  entryPrice,
  bid,
  ask,
  ordersActive,
  lines,
  annotations,
  alarms,
  linePoint1,
  indicators = [],
  markers = [],
  crosshairSync,
  paneId,
  onPriceClick,
  onSlDragEnd,
  onTpDragEnd,
  onSlDblClick,
  onDeleteSl,
  onDeleteTp,
  onDeleteAlarm,
}: Props) {
  const containerRef  = useRef<HTMLDivElement>(null);
  const chartRef      = useRef<IChartApi | null>(null);
  const candleRef     = useRef<ISeriesApi<"Candlestick"> | null>(null);
  const slLineRef     = useRef<IPriceLine | null>(null);
  const tpLineRef     = useRef<IPriceLine | null>(null);
  const entryLineRef  = useRef<IPriceLine | null>(null);
  const bidLineRef    = useRef<IPriceLine | null>(null);
  const askLineRef    = useRef<IPriceLine | null>(null);
  const lineSeriesMap = useRef<Map<string, ISeriesApi<"Line">>>(new Map());
  // User-placed price alarms (amber dashed lines), keyed by alarm id.
  const alarmLineMap  = useRef<Map<string, IPriceLine>>(new Map());
  // Previous-day reference levels (PDC/PDH/PDL), keyed by indicator kind.
  const prevDayLineMap = useRef<Map<string, IPriceLine>>(new Map());
  // Strategy-card indicator series (VWAP / EMA / SMA = Line), keyed by
  // indicatorId so they can be reconciled in place.
  const indicatorSeriesMap = useRef<Map<string, ISeriesApi<"Line"> | ISeriesApi<"Histogram">>>(new Map());
  // Volume histogram — always drawn on every pane (bottom overlay), independent
  // of the strategy card's indicators.
  const volumeSeriesRef = useRef<ISeriesApi<"Histogram"> | null>(null);
  // Full-height background histogram tinting pre/post-market (extended hours).
  const sessionBgRef    = useRef<ISeriesApi<"Histogram"> | null>(null);
  // Trade-execution triangles + connecting P&L line (custom series primitive).
  const execPrimRef     = useRef<ExecutionsPrimitive | null>(null);
  // On-chart ✕ delete buttons for user price lines (SL/TP/alarms): DOM nodes
  // keyed by tag, positioned imperatively each frame from their line's price.
  const tagBtnRefs      = useRef<Map<string, HTMLButtonElement>>(new Map());
  const tagPriceRef     = useRef<Map<string, number>>(new Map());

  // Always-fresh refs for callbacks and props — avoids stale closures
  const onPriceClickRef = useRef(onPriceClick);
  const onSlDragEndRef  = useRef(onSlDragEnd);
  const onTpDragEndRef  = useRef(onTpDragEnd);
  const onSlDblClickRef = useRef(onSlDblClick);
  const drawModeRef     = useRef<DrawMode>(drawMode);
  const slPriceRef      = useRef<number | null>(slPrice);
  const tpPriceRef      = useRef<number | null>(tpPrice);
  const ordersActiveRef = useRef<boolean>(ordersActive);
  const symbolRef       = useRef<string>(symbol);
  const timeframeRef    = useRef<Timeframe>(timeframe);
  const barsRef         = useRef<Awaited<ReturnType<typeof api.getTickerBars>>>();
  // Accumulated bar history (keyed by Unix-seconds): live poll upserts the recent
  // bars (live wins) while scroll-back lazily prepends older batches, so the
  // chart keeps a continuous, gap-free series the further you zoom out.
  const accumRef        = useRef<Map<number, Bar>>(new Map());
  const loadingOlderRef = useRef<boolean>(false);
  const noMoreOlderRef  = useRef<boolean>(false);
  // The current (last) candle, kept in sync so live ticks can update it.
  const lastBarRef      = useRef<CandlestickData | null>(null);
  // What has actually been pushed to the candle series, so the render effect can
  // tell a routine live-tail change (→ series.update, view untouched) from an
  // initial load or an older-history prepend (→ series.setData). This is the
  // official lightweight-charts protocol: setData once + on prepend, update() for
  // realtime. Calling setData every poll was resetting the user's pan/zoom.
  const renderedFirstRef = useRef<number | null>(null);
  const renderedLastRef  = useRef<number | null>(null);

  useEffect(() => { onPriceClickRef.current = onPriceClick; });
  useEffect(() => { onSlDragEndRef.current  = onSlDragEnd; });
  useEffect(() => { onTpDragEndRef.current  = onTpDragEnd; });
  useEffect(() => { onSlDblClickRef.current = onSlDblClick; });
  useEffect(() => { drawModeRef.current     = drawMode; });
  useEffect(() => { slPriceRef.current      = slPrice; });
  useEffect(() => { tpPriceRef.current      = tpPrice; });
  useEffect(() => { ordersActiveRef.current = ordersActive; });
  useEffect(() => { symbolRef.current       = symbol; });
  useEffect(() => { timeframeRef.current     = timeframe; });

  // Drag state
  const dragging = useRef<"sl" | "tp" | null>(null);

  const queryClient = useQueryClient();

  // ── Unified bar load (refresh on open) ─────────────────────────────────────
  // The single bar-loading path for every pane / strategy / timeframe: on open
  // (and on a slow interval) the backend refreshes the history from Alpaca —
  // filling gaps and pulling today's still-forming session bar (incl. daily) —
  // and merges it into RAM. The fast `get_ticker_bars` poll below then renders
  // the live RAM series. Sub-minute timeframes Alpaca can't serve just return
  // RAM, so we don't bother hitting it on an interval for those.
  const { data: backfilled } = useQuery({
    queryKey: ["chart_history", symbol, timeframe],
    queryFn:  () => api.loadChartBars(symbol, timeframe),
    enabled:  !!symbol,
    refetchOnMount: "always",
    refetchOnWindowFocus: false,
    refetchInterval:
      timeframe === "5s" || timeframe === "10s" ? false
      : timeframe === "daily" ? 30_000
      : 15_000,
    retry: false,
  });
  // Once a refresh lands, nudge the live bars query so it renders immediately.
  useEffect(() => {
    if (backfilled && backfilled.length > 0) {
      queryClient.invalidateQueries({ queryKey: ["bars", symbol, timeframe] });
    }
  }, [backfilled, symbol, timeframe, queryClient]);

  // ── Poll live bars from RAM (cheap, fast) ──────────────────────────────────
  const { data: fetchedBars } = useQuery({
    queryKey: ["bars", symbol, timeframe],
    queryFn:  () => api.getTickerBars(symbol, timeframe),
    refetchInterval: timeframe === "5s" || timeframe === "10s" ? 500 : 1000,
    enabled:  !!symbol,
  });

  // The rendered series = the accumulated history (older back-fill + live tail).
  const [bars, setBars] = useState<Bar[] | undefined>(undefined);
  useEffect(() => { barsRef.current = bars; }, [bars]);

  // Reset the accumulator when the symbol or timeframe changes (different series).
  useEffect(() => {
    accumRef.current = new Map();
    loadingOlderRef.current = false;
    noMoreOlderRef.current  = false;
    // Force a full setData (not an update) for the first render of the new series.
    renderedFirstRef.current = null;
    renderedLastRef.current  = null;
    setBars(undefined);
  }, [symbol, timeframe]);

  // Merge each live poll into the accumulator (live bars win for their slot) and
  // re-render the full ascending series.
  useEffect(() => {
    if (!fetchedBars?.length) return;
    const accum = accumRef.current;
    for (const b of fetchedBars) accum.set(toUTC(b.time) as number, b);
    setBars([...accum.entries()].sort((a, c) => a[0] - c[0]).map((e) => e[1]));
  }, [fetchedBars]);

  // ── Lazy back-fill of older history (scroll/zoom into the past) ─────────────
  // When the visible range nears the left edge, fetch a batch of older bars from
  // Alpaca (ending before the oldest loaded bar) and prepend them, filling the
  // blank. Guarded against concurrent / dead-end fetches.
  const loadOlderBars = useCallback(async () => {
    const sym = symbolRef.current;
    const tf  = timeframeRef.current;
    if (loadingOlderRef.current || noMoreOlderRef.current) return;
    if (tf === "5s" || tf === "10s") return; // Alpaca REST can't serve sub-minute
    if (accumRef.current.size === 0) return;

    loadingOlderRef.current = true;
    try {
      // Fetch older bars in batches until the visible left edge has a comfortable
      // lead of loaded bars. The chart can't re-render mid-loop (data lands via the
      // render effect below, asynchronously), so we read the visible range once and
      // estimate the new left-edge index from the bars we've prepended. After the
      // setData below, lightweight-charts preserves the visible TIME range, so the
      // logical `from` jumps up by the prepended count and the next range-change
      // event only re-fires if the user keeps scrolling — self-correcting.
      const startRange = chartRef.current?.timeScale().getVisibleLogicalRange();
      const from0  = startRange ? startRange.from : 0;
      // How many bars of lead we want past the trigger threshold once done.
      const target = BACKFILL_THRESHOLD + BACKFILL_BATCH;
      const accum  = accumRef.current;
      let totalAdded = 0;

      for (let i = 0; i < 10; i++) {
        // Oldest currently-loaded bar = the back-fill cutoff.
        let oldestSec = Infinity;
        let oldestIso = "";
        for (const [sec, b] of accum) {
          if (sec < oldestSec) { oldestSec = sec; oldestIso = b.time; }
        }
        if (!oldestIso) break;

        const older = await api.loadOlderBars(sym, tf, oldestIso, BACKFILL_BATCH);
        // The chart may have switched symbol/timeframe while the request was in
        // flight — discard the stale batch rather than poison the new accumulator.
        if (symbolRef.current !== sym || timeframeRef.current !== tf) return;
        if (!older?.length) { noMoreOlderRef.current = true; break; }

        let added = 0;
        for (const b of older) {
          const sec = toUTC(b.time) as number;
          if (!accum.has(sec)) { accum.set(sec, b); added++; }
        }
        if (added === 0) { noMoreOlderRef.current = true; break; } // reached data start
        totalAdded += added;
        if (from0 + totalAdded > target) break;
      }

      // One render after the whole batch: the render effect sees a changed FIRST
      // timestamp → full setData (which keeps the user's view) instead of update().
      if (totalAdded > 0) {
        setBars([...accum.entries()].sort((a, c) => a[0] - c[0]).map((e) => e[1]));
      }
    } catch { /* soft-fail */ }
    finally { loadingOlderRef.current = false; }
  }, []);

  // ── Previous-day reference levels (PDC/PDH/PDL) ────────────────────────────
  // Fetched only when the pane requests a previous_* indicator. The backend
  // returns the previous TRADING day's close/high/low relative to today's date,
  // so a cached partial bar for the current session is never used by mistake.
  const wantsPrevDay = indicators.some(
    (i) => i.kind === "previous_close" || i.kind === "previous_high" || i.kind === "previous_low",
  );
  const { data: prevDay } = useQuery({
    queryKey: ["prev_day_levels", symbol],
    queryFn:  () => api.getPreviousDayLevels(symbol),
    enabled:  !!symbol && wantsPrevDay,
    staleTime: 5 * 60 * 1000,
  });

  // ── Trade executions (triangles + P&L line) ────────────────────────────────
  // Persisted per ticker (multi-day), shown on every chart of that symbol. Polled
  // so new fills appear live. Pushed into the series primitive on change.
  const { data: executions } = useQuery({
    queryKey: ["executions", symbol],
    queryFn:  () => api.getExecutionsForSymbol(symbol),
    enabled:  !!symbol,
    refetchInterval: 2000,
  });
  // Re-push on executions OR bars change: fills are snapped to bar times, so the
  // primitive needs the current bar set (updated as bars stream / back-fill).
  useEffect(() => {
    const times = bars?.map((b) => toUTC(b.time) as number) ?? [];
    execPrimRef.current?.setData(executions ?? [], times);
  }, [executions, bars]);

  // ── Create chart once ──────────────────────────────────────────────────────
  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;

    const chart = createChart(container, {
      width:  container.clientWidth,
      height: container.clientHeight,
      layout: {
        background:  { color: "#080808" },
        textColor:   "#666",
        fontSize:    10,
        fontFamily:  "ui-monospace, monospace",
      },
      // All time labels (axis + crosshair) render in New York time. Timestamps
      // stay real-UTC; only the display is converted via Intl (DST-aware).
      localization: {
        timeFormatter: (t: Time) =>
          nyDateTime(Number(t), timeframe === "5s" || timeframe === "10s"),
      },
      grid: {
        vertLines: { color: "#111" },
        horzLines: { color: "#111" },
      },
      crosshair: {
        mode:     CrosshairMode.Normal,
        vertLine: { color: "#333", labelBackgroundColor: "#1a1a1a" },
        horzLine: { color: "#333", labelBackgroundColor: "#1a1a1a" },
      },
      timeScale: {
        borderColor:      "#1e1e1e",
        timeVisible:      true,
        secondsVisible:   timeframe === "5s" || timeframe === "10s",
        // Leave a 5-bar gap on the right so the latest candle isn't glued to the
        // price axis edge.
        rightOffset:      5,
        fixLeftEdge:      false,
        fixRightEdge:     false,
        // Axis tick labels in New York time.
        tickMarkFormatter: (t: Time, tickMarkType: TickMarkType) => {
          const sec = Number(t);
          switch (tickMarkType) {
            case TickMarkType.Year:            return nyYear(sec);
            case TickMarkType.Month:           return nyMonth(sec);
            case TickMarkType.DayOfMonth:      return nyDayMonth(sec);
            case TickMarkType.TimeWithSeconds: return nyTime(sec, true);
            default:                           return nyTime(sec, false);
          }
        },
      },
      rightPriceScale: {
        borderColor:  "#1e1e1e",
        scaleMargins: { top: 0.08, bottom: 0.08 },
      },
      handleScroll: true,
      // Disable the built-in mouse-wheel zoom (it anchors on the cursor). We
      // drive wheel zoom manually below via barSpacing so it stays anchored on
      // the latest bar (right edge) and only expands to the left. Axis-drag and
      // pinch scaling keep their default behaviour.
      handleScale: {
        mouseWheel:           false,
        pinch:                true,
        axisPressedMouseMove: true,
        axisDoubleClickReset: true,
      },
    });

    // Extended-hours (pre/post-market) background shading. Created BEFORE the
    // candle series so it paints behind everything. A full-height histogram on
    // its own hidden price scale, tinted only on bars outside the cash session.
    const sessionBg = chart.addHistogramSeries({
      priceScaleId:     "session-bg",
      base:             0,
      priceLineVisible: false,
      lastValueVisible: false,
    });
    sessionBg.priceScale().applyOptions({ scaleMargins: { top: 0, bottom: 0 } });
    sessionBgRef.current = sessionBg;

    const candle = chart.addCandlestickSeries({
      upColor:          "#26a69a",
      downColor:        "#ef5350",
      borderUpColor:    "#26a69a",
      borderDownColor:  "#ef5350",
      wickUpColor:      "#26a69a",
      wickDownColor:    "#ef5350",
      priceLineVisible: false,
    });

    chartRef.current  = chart;
    candleRef.current = candle;

    // Trade-execution markers (triangles + P&L line) as a series primitive.
    const execPrim = new ExecutionsPrimitive();
    candle.attachPrimitive(execPrim);
    execPrimRef.current = execPrim;

    // ── Single click: place SL / TP / alarm / line point / annotation ──────
    // A single click places the active tool (no double-click needed). When no
    // tool is active (drawMode "none") the parent handler is a no-op, so a plain
    // click just moves the crosshair as usual. The SL/TP lines are drawn
    // IMMEDIATELY here; onPriceClickRef notifies the parent for Rust persistence
    // + drawMode reset. Alarms / lines / text are handled by the parent.
    chart.subscribeClick((param) => {
      if (drawModeRef.current === "none") return;
      if (!param.point) return;

      const price = candle.coordinateToPrice(param.point.y);
      if (price == null) return;

      const mode = drawModeRef.current;

      if (mode === "sl") {
        // Create or update the SL price line directly on the series
        if (slLineRef.current) {
          slLineRef.current.applyOptions({ price });
        } else {
          slLineRef.current = candle.createPriceLine({ price, ...slOpts(ordersActiveRef.current) });
        }
        slPriceRef.current = price;
      } else if (mode === "tp") {
        // Calculate R-ratio title if SL is known
        let title = "TP";
        const sl   = slPriceRef.current;
        const bars = barsRef.current;
        if (sl != null && bars?.length) {
          const entry = bars[bars.length - 1].close;
          const risk  = Math.abs(entry - sl);
          if (risk > 0) title = `TP  ${(Math.abs(price - entry) / risk).toFixed(1)}R`;
        }
        if (tpLineRef.current) {
          tpLineRef.current.applyOptions({ price, title });
        } else {
          tpLineRef.current = candle.createPriceLine({ price, ...tpOpts(ordersActiveRef.current, title) });
        }
        tpPriceRef.current = price;
      }

      // Notify parent: persist to Rust + reset drawMode
      const time = typeof param.time === "number" ? param.time : 0;
      onPriceClickRef.current(price, time, param.point.x, param.point.y);
    });

    // ── Double click: SL shortcut ──────────────────────────────────────────
    // Memorise the clicked price as a (provisional) SL without first arming the
    // SL tool — this unlocks the 25/50/100 order buttons. A second double-click
    // simply moves the SL to the new price. The line is drawn IMMEDIATELY here;
    // onSlDblClickRef notifies the parent to persist it (updateZoneSl), which
    // also derives position size + long/short on the next order.
    chart.subscribeDblClick((param) => {
      if (!param.point) return;
      const price = candle.coordinateToPrice(param.point.y);
      if (price == null) return;

      if (slLineRef.current) {
        slLineRef.current.applyOptions({ price });
      } else {
        slLineRef.current = candle.createPriceLine({ price, ...slOpts(ordersActiveRef.current) });
      }
      slPriceRef.current = price;

      onSlDblClickRef.current(price);
    });

    // ── Global crosshair sync (same instrument, sibling panes) ────────────────
    // On hover, broadcast the FREE price under the cursor (coordinateToPrice of
    // the pointer's y — NOT snapped to OHLC) + the time, so every other same-
    // symbol pane mirrors the crosshair at the exact same price via
    // setCrosshairPosition; on leave, broadcast a clear. `sync.syncing` guards
    // against the programmatic echo (a mirrored set re-emitting a move event) so
    // it can't loop. Official API: lightweight-charts "Set crosshair position".
    if (crosshairSync && paneId) {
      chart.subscribeCrosshairMove((param) => {
        if (crosshairSync.syncing) return;
        const price = param.point ? candle.coordinateToPrice(param.point.y) : null;
        crosshairSync.broadcast(
          paneId,
          symbolRef.current,
          param.time != null && price != null ? param.time : null,
          price ?? null,
        );
      });
    }

    // ── Lazy older-history back-fill on scroll/zoom into the past ──────────────
    // When the left edge of the visible range approaches the first loaded bar,
    // fetch a batch of older bars and prepend them (handled in loadOlderBars).
    chart.timeScale().subscribeVisibleLogicalRangeChange((range) => {
      // Official infinite-history hook: when the left edge nears (or passes) the
      // first loaded bar, pull older bars from Alpaca so a zoom-out never shows a
      // blank. Threshold > 0 pre-loads slightly before the edge is actually hit.
      if (range && range.from < BACKFILL_THRESHOLD) void loadOlderBars();
    });

    // ── Wheel zoom anchored on the latest bar ─────────────────────────────────
    // Changing barSpacing through the official API keeps the time scale's right
    // offset fixed, so the right-most (current) bar stays put and the zoom only
    // expands/contracts to the left — instead of zooming around the cursor.
    const onWheel = (e: WheelEvent) => {
      e.preventDefault();
      const ts   = chart.timeScale();
      const opts = ts.options();
      // One notch ≈ 10% in/out. deltaY < 0 = wheel up = zoom in (wider bars).
      const factor  = e.deltaY < 0 ? 1.1 : 1 / 1.1;
      const next    = Math.max(opts.minBarSpacing, Math.min(opts.barSpacing * factor, 80));
      ts.applyOptions({ barSpacing: next });
    };
    container.addEventListener("wheel", onWheel, { passive: false });

    const ro = new ResizeObserver(() => {
      // When the zone's tab is hidden (display:none) the container collapses to
      // 0×0. Skip resizing to zero so the chart keeps its layout and visible
      // logical range (pan/zoom) while hidden, and restores cleanly when shown.
      const w = container.clientWidth;
      const h = container.clientHeight;
      if (w > 0 && h > 0 && chartRef.current) {
        chartRef.current.resize(w, h);
      }
    });
    ro.observe(container);

    return () => {
      container.removeEventListener("wheel", onWheel);
      ro.disconnect();
      chart.remove();
      chartRef.current     = null;
      candleRef.current    = null;
      slLineRef.current    = null;
      tpLineRef.current    = null;
      entryLineRef.current = null;
      bidLineRef.current   = null;
      askLineRef.current   = null;
      lineSeriesMap.current.clear();
      indicatorSeriesMap.current.clear();
      volumeSeriesRef.current = null;
      sessionBgRef.current = null;
      execPrimRef.current = null;
      alarmLineMap.current.clear();
      prevDayLineMap.current.clear();
    };
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  // ── Register this pane in the crosshair sync group ─────────────────────────
  // Re-registers on symbol change so a zone re-assignment keeps the same-symbol
  // matching correct. apply/clear read the chart refs lazily at broadcast time.
  useEffect(() => {
    if (!crosshairSync || !paneId) return;
    return crosshairSync.register(paneId, {
      symbol,
      apply: (time, price) => {
        const chart = chartRef.current;
        const candle = candleRef.current;
        if (!chart || !candle) return;
        try { chart.setCrosshairPosition(price, time, candle); } catch { /* out-of-range */ }
      },
      clear: () => {
        try { chartRef.current?.clearCrosshairPosition(); } catch { /* ignore */ }
      },
    });
  }, [crosshairSync, paneId, symbol]);

  // ── Feed bar data (official lightweight-charts protocol) ───────────────────
  // setData() is reserved for the initial load and for older-history prepends
  // (front of the series changed) — lightweight-charts preserves the visible TIME
  // range across setData, so a prepend keeps the user's pan/zoom while exposing
  // the new bars to the left. Routine live polls (front unchanged, only the tail
  // grows / the last candle mutates) go through series.update(), which never
  // touches the view. Calling setData on every poll — as before — was snapping the
  // user back whenever they scrolled into the past, so zoom-out felt "stuck".
  useEffect(() => {
    const series = candleRef.current;
    if (!series || !bars?.length) return;

    const data: CandlestickData[] = bars.map((b) => ({
      time:  toUTC(b.time),
      open:  b.open,
      high:  b.high,
      low:   b.low,
      close: b.close,
    }));
    const firstSec = data[0].time as number;
    const lastSec  = data[data.length - 1].time as number;
    const prevFirst = renderedFirstRef.current;
    const prevLast  = renderedLastRef.current;

    // Full replace when: first render of this series, the front changed (older
    // bars prepended), or the tail went backwards (series reset / shrank).
    const needsFull =
      prevFirst == null ||
      prevLast  == null ||
      firstSec !== prevFirst ||
      lastSec  <  prevLast;

    if (needsFull) {
      try { series.setData(data); } catch { /* duplicate-time guard */ }
    } else {
      // Tail-only change: update the previously-last bar (it may have just closed)
      // and any bars appended since, in ascending order. update() requires times
      // ≥ the series' last bar, which is exactly the prevLast..end slice.
      let start = data.length - 1;
      while (start > 0 && (data[start - 1].time as number) >= prevLast) start--;
      for (let i = start; i < data.length; i++) {
        try { series.update(data[i]); } catch { /* out-of-order guard */ }
      }
    }

    renderedFirstRef.current = firstSec;
    renderedLastRef.current  = lastSec;
    lastBarRef.current = data[data.length - 1];
  }, [bars]);

  // ── Live ticks (focus symbols) — update the forming candle in place ────────
  // The backend pushes `market-tick` for displayed symbols; we move the current
  // candle to the tick price (or open a new candle when the bucket flips), so
  // the chart is truly tick-by-tick instead of waiting for a poll/close.
  // Skipped on the daily pane: its day bar is authoritative from Alpaca, and a
  // tick-built daily candle would only reflect ticks seen since connect.
  useEffect(() => {
    if (timeframe === "daily") return;
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    listen<{ symbol: string; price: number; ts: number }>("market-tick", (e) => {
      const t = e.payload;
      if (t.symbol !== symbol) return;
      const series = candleRef.current;
      if (!series) return;
      const secs   = TF_SECONDS[timeframe] ?? 60;
      const bucket = (Math.floor(t.ts / secs) * secs) as UTCTimestamp;
      const last   = lastBarRef.current;
      const bar: CandlestickData =
        last && (last.time as number) === bucket
          ? { time: bucket, open: last.open, high: Math.max(last.high, t.price), low: Math.min(last.low, t.price), close: t.price }
          : (last && (last.time as number) > bucket)
            ? last // out-of-order tick — ignore
            : { time: bucket, open: t.price, high: t.price, low: t.price, close: t.price };
      lastBarRef.current = bar;
      try { series.update(bar); } catch { /* out-of-range guard */ }
    }).then((fn) => { if (cancelled) fn(); else unlisten = fn; });
    return () => { cancelled = true; unlisten?.(); };
  }, [symbol, timeframe]);

  // ── Indicators (strategy-card driven) ──────────────────────────────────────
  // Reconcile the requested indicator series against what's currently drawn,
  // then (re)compute their values from the loaded bars. VWAP uses Bar.vwap;
  // EMA/SMA are computed client-side; Volume is a bottom-pinned histogram.
  const indicatorsKey = indicators.map(indicatorId).join(",");
  useEffect(() => {
    const chart = chartRef.current;
    if (!chart) return;

    // Desired series ids. Bollinger expands to three sub-series (upper/basis/
    // lower); previous-day levels are price lines (drawn elsewhere), not series.
    const desired = new Set<string>();
    for (const ind of indicators) {
      const id = indicatorId(ind);
      if (ind.kind === "bollinger_bands") {
        desired.add(`${id}:u`); desired.add(`${id}:m`); desired.add(`${id}:l`);
      } else if (
        ind.kind === "previous_close" ||
        ind.kind === "previous_high" ||
        ind.kind === "previous_low" ||
        ind.kind === "volume" // volume is always drawn (own effect), not here
      ) {
        // not a series managed by this reconcile loop
      } else {
        desired.add(id);
      }
    }
    for (const [id, series] of indicatorSeriesMap.current) {
      if (!desired.has(id)) {
        chart.removeSeries(series);
        indicatorSeriesMap.current.delete(id);
      }
    }

    if (!bars?.length) return;

    const times  = bars.map((b) => toUTC(b.time));
    const closes = bars.map((b) => b.close);

    for (const ind of indicators) {
      const id = indicatorId(ind);

      // Previous-day reference levels are horizontal price lines drawn in a
      // dedicated effect (they need the prior daily bar, not this pane's series).
      if (
        ind.kind === "previous_close" ||
        ind.kind === "previous_high" ||
        ind.kind === "previous_low"
      ) {
        continue;
      }

      if (ind.kind === "bollinger_bands") {
        const period = ind.period ?? 20;
        const { upper, basis, lower } = computeBollinger(closes, period, BOLLINGER_K);
        const parts: [string, (number | null)[], string][] = [
          [`${id}:u`, upper, BOLLINGER_COLORS.band],
          [`${id}:m`, basis, BOLLINGER_COLORS.basis],
          [`${id}:l`, lower, BOLLINGER_COLORS.band],
        ];
        for (const [sid, vals, color] of parts) {
          let series = indicatorSeriesMap.current.get(sid) as ISeriesApi<"Line"> | undefined;
          if (!series) {
            series = chart.addLineSeries({
              color,
              lineWidth:              1,
              priceLineVisible:       false,
              lastValueVisible:       false,
              crosshairMarkerVisible: false,
            });
            indicatorSeriesMap.current.set(sid, series);
          }
          const data: LineData[] = [];
          vals.forEach((v, i) => { if (v != null) data.push({ time: times[i], value: v }); });
          try { series.setData(data); } catch { /* duplicate-time guard */ }
        }
        continue;
      }

      // Volume is drawn unconditionally on every pane by a dedicated effect
      // below, so the card's `volume` indicator (if any) is a no-op here.
      if (ind.kind === "volume") continue;

      // Line indicators: vwap / ema / sma
      let series = indicatorSeriesMap.current.get(id) as ISeriesApi<"Line"> | undefined;
      if (!series) {
        series = chart.addLineSeries({
          color:                  INDICATOR_COLORS[ind.kind as keyof typeof INDICATOR_COLORS] ?? "#888",
          lineWidth:              1,
          priceLineVisible:       false,
          lastValueVisible:       false,
          crosshairMarkerVisible: false,
        });
        indicatorSeriesMap.current.set(id, series);
      }

      let values: (number | null)[];
      if (ind.kind === "vwap")      values = bars.map((b) => b.vwap);
      else if (ind.kind === "ema")  values = computeEma(closes, ind.period ?? 9);
      else if (ind.kind === "sma")  values = computeSma(closes, ind.period ?? 20);
      else                           values = bars.map(() => null); // previous_close: not plotted here

      const data: LineData[] = [];
      values.forEach((v, i) => { if (v != null) data.push({ time: times[i], value: v }); });
      try { series.setData(data); } catch { /* duplicate-time guard */ }
    }
  }, [bars, indicatorsKey]); // eslint-disable-line react-hooks/exhaustive-deps

  // ── Volume histogram — always on, every pane ───────────────────────────────
  // A bottom-pinned overlay histogram fed from the loaded bars, shown by default
  // on every chart / strategy / pane (no strategy-card opt-in needed).
  useEffect(() => {
    const chart = chartRef.current;
    if (!chart) return;
    if (!bars?.length) return;
    if (!volumeSeriesRef.current) {
      volumeSeriesRef.current = chart.addHistogramSeries({
        priceFormat:      { type: "volume" },
        priceScaleId:     "", // overlay scale, pinned to the bottom below
        priceLineVisible: false,
        lastValueVisible: false,
      });
      volumeSeriesRef.current.priceScale().applyOptions({ scaleMargins: { top: 0.82, bottom: 0 } });
    }
    const data = bars.map((b) => ({
      time:  toUTC(b.time),
      value: b.volume,
      color: b.close >= b.open ? "rgba(38,166,154,0.4)" : "rgba(239,83,80,0.4)",
    }));
    try { volumeSeriesRef.current.setData(data); } catch { /* duplicate-time guard */ }
  }, [bars]);

  // ── Pre/post-market background shading ─────────────────────────────────────
  // A barely-visible (5% opacity) full-height tint behind the candles on bars
  // that fall outside the 09:30–16:00 NY cash session. Intraday only — a daily
  // bar spans a whole day, so session shading doesn't apply.
  useEffect(() => {
    const series = sessionBgRef.current;
    if (!series) return;
    if (timeframe === "daily" || !bars?.length) {
      try { series.setData([]); } catch { /* */ }
      return;
    }
    const TINT  = "rgba(130,150,190,0.02)"; // barely-there blue-grey, extended hours
    const CLEAR = "rgba(0,0,0,0)";          // transparent during the cash session
    const data = bars.map((b) => ({
      time:  toUTC(b.time),
      value: 1, // constant → fills full pane height on its own hidden scale
      color: isExtendedHours(b.time) ? TINT : CLEAR,
    }));
    try { series.setData(data); } catch { /* duplicate-time guard */ }
  }, [bars, timeframe]);

  // ── Candle markers (e.g. red dots on split days) ───────────────────────────
  const markersKey = markers.map((m) => `${m.time}:${m.color}`).join(",");
  useEffect(() => {
    const series = candleRef.current;
    if (!series) return;
    const data: SeriesMarker<Time>[] = markers.map((m) => ({
      time:     m.time as UTCTimestamp,
      position: "belowBar",
      color:    m.color,
      shape:    "circle",
      text:     m.text,
    }));
    try { series.setMarkers(data); } catch { /* ignore out-of-range marker */ }
  }, [markersKey]); // eslint-disable-line react-hooks/exhaustive-deps

  // ── SL price line — sync from parent state ─────────────────────────────────
  // This effect handles loading SL from Rust on startup / zone switch.
  // When the user draws a new SL via double-click, subscribeDblClick already
  // called createPriceLine/applyOptions; this effect then just confirms the
  // price with applyOptions (no visible change, no duplicate line).
  useEffect(() => {
    const series = candleRef.current;
    if (!series) return;
    if (slPrice == null) {
      if (slLineRef.current) {
        series.removePriceLine(slLineRef.current);
        slLineRef.current = null;
      }
      return;
    }
    if (slLineRef.current) {
      slLineRef.current.applyOptions({ price: slPrice });
    } else {
      slLineRef.current = series.createPriceLine({ price: slPrice, ...slOpts(ordersActive) });
    }
  }, [slPrice]); // eslint-disable-line react-hooks/exhaustive-deps

  // ── TP price line — create / remove based on Rust state ──────────────────
  // deps: [tpPrice] ONLY — bars refetch must NOT trigger removal during the
  // window between dblclick (visual line created) and Rust response (tpPrice
  // prop update). The bars dep was the root cause of the "TP disappears after
  // a few seconds" bug.
  useEffect(() => {
    const series = candleRef.current;
    if (!series) return;
    if (tpPrice == null) {
      if (tpLineRef.current) {
        series.removePriceLine(tpLineRef.current);
        tpLineRef.current = null;
      }
      return;
    }
    if (tpLineRef.current) {
      tpLineRef.current.applyOptions({ price: tpPrice });
    } else {
      tpLineRef.current = series.createPriceLine({ price: tpPrice, ...tpOpts(ordersActive) });
    }
  }, [tpPrice]); // eslint-disable-line react-hooks/exhaustive-deps

  // ── Toggle SL/TP between "planned" (solid) and "order" (dotted) styling ────
  // when a position opens/closes. Only color + lineStyle are touched so the TP
  // R-ratio title and the line price are preserved.
  useEffect(() => {
    if (slLineRef.current) {
      const o = slOpts(ordersActive);
      slLineRef.current.applyOptions({ color: o.color, lineStyle: o.lineStyle });
    }
    if (tpLineRef.current) {
      const o = tpOpts(ordersActive);
      tpLineRef.current.applyOptions({ color: o.color, lineStyle: o.lineStyle });
    }
  }, [ordersActive]);

  // ── TP R-ratio title — update live as bars / slPrice change ───────────────
  // Separate from create/remove so that bars updates only touch the title,
  // never triggering line deletion.
  useEffect(() => {
    const line = tpLineRef.current;
    if (!line || slPrice == null || !bars?.length) return;
    const tp = tpPrice ?? tpPriceRef.current;
    if (tp == null) return;
    const entry = bars[bars.length - 1].close;
    const risk  = Math.abs(entry - slPrice);
    if (risk <= 0) return;
    line.applyOptions({ title: `TP  ${(Math.abs(tp - entry) / risk).toFixed(1)}R` });
  }, [tpPrice, slPrice, bars]);

  // ── Entry price line ───────────────────────────────────────────────────────
  useEffect(() => {
    const series = candleRef.current;
    if (!series) return;
    if (entryPrice == null) {
      if (entryLineRef.current) {
        series.removePriceLine(entryLineRef.current);
        entryLineRef.current = null;
      }
      return;
    }
    if (entryLineRef.current) {
      entryLineRef.current.applyOptions({ price: entryPrice });
    } else {
      entryLineRef.current = series.createPriceLine({ price: entryPrice, ...ENTRY_OPTIONS });
    }
  }, [entryPrice]);

  // ── Bid / Ask price lines ─────────────────────────────────────────────────
  useEffect(() => {
    const series = candleRef.current;
    if (!series) return;
    if (bid != null) {
      if (bidLineRef.current) {
        bidLineRef.current.applyOptions({ price: bid });
      } else {
        bidLineRef.current = series.createPriceLine({ price: bid, ...BID_ASK_OPTIONS, title: "Bid" });
      }
    } else if (bidLineRef.current) {
      series.removePriceLine(bidLineRef.current);
      bidLineRef.current = null;
    }
    if (ask != null) {
      if (askLineRef.current) {
        askLineRef.current.applyOptions({ price: ask });
      } else {
        askLineRef.current = series.createPriceLine({ price: ask, ...BID_ASK_OPTIONS, title: "Ask" });
      }
    } else if (askLineRef.current) {
      series.removePriceLine(askLineRef.current);
      askLineRef.current = null;
    }
  }, [bid, ask]);

  // ── Drawing lines (ChartLine[]) ────────────────────────────────────────────
  useEffect(() => {
    if (!chartRef.current) return;
    const currentIds = new Set(lines.map((l) => l.id));
    for (const [id, series] of lineSeriesMap.current) {
      if (!currentIds.has(id)) {
        chartRef.current.removeSeries(series);
        lineSeriesMap.current.delete(id);
      }
    }
    for (const line of lines) {
      if (!lineSeriesMap.current.has(line.id)) {
        const s = chartRef.current.addLineSeries({
          color:                  "#f59e0b",
          lineWidth:              1,
          lineStyle:              LineStyle.Solid,
          priceLineVisible:       false,
          lastValueVisible:       false,
          crosshairMarkerVisible: false,
        });
        const pts: LineData[] = [
          { time: line.point1.time as UTCTimestamp, value: line.point1.price },
          { time: line.point2.time as UTCTimestamp, value: line.point2.price },
        ];
        if (pts[0].time > pts[1].time) pts.reverse();
        try { s.setData(pts); } catch { /* skip malformed */ }
        lineSeriesMap.current.set(line.id, s);
      }
    }
  }, [lines]);

  // ── Price alarms (amber dashed price lines, reconciled by id) ──────────────
  const alarmsKey = alarms.map((a) => `${a.id}:${a.price}`).join(",");
  useEffect(() => {
    const series = candleRef.current;
    if (!series) return;
    const wanted = new Set(alarms.map((a) => a.id));
    for (const [id, line] of alarmLineMap.current) {
      if (!wanted.has(id)) {
        series.removePriceLine(line);
        alarmLineMap.current.delete(id);
      }
    }
    for (const a of alarms) {
      const existing = alarmLineMap.current.get(a.id);
      if (existing) {
        existing.applyOptions({ price: a.price });
      } else {
        alarmLineMap.current.set(a.id, series.createPriceLine({ price: a.price, ...ALARM_OPTIONS }));
      }
    }
  }, [alarmsKey]); // eslint-disable-line react-hooks/exhaustive-deps

  // ── On-chart ✕ delete buttons for user price lines (SL/TP/alarms) ──────────
  // lightweight-charts has no native delete affordance on price lines, so we
  // overlay a tiny ✕ aligned to each user line. The price→pixel map is rebuilt
  // when the lines change; an rAF loop repositions the buttons every frame so
  // they track the line through scroll / zoom / autoscale.
  useEffect(() => {
    const m = new Map<string, number>();
    if (slPrice != null && onDeleteSl) m.set("sl", slPrice);
    if (tpPrice != null && onDeleteTp) m.set("tp", tpPrice);
    if (onDeleteAlarm) for (const a of alarms) m.set(`alarm-${a.id}`, a.price);
    tagPriceRef.current = m;
  }, [slPrice, tpPrice, alarmsKey, onDeleteSl, onDeleteTp, onDeleteAlarm]); // eslint-disable-line react-hooks/exhaustive-deps

  useEffect(() => {
    let raf = 0;
    const tick = () => {
      const candle = candleRef.current;
      if (candle) {
        for (const [key, node] of tagBtnRefs.current) {
          const price = tagPriceRef.current.get(key);
          const y = price != null ? candle.priceToCoordinate(price) : null;
          if (y == null) {
            node.style.display = "none";
          } else {
            node.style.display = "flex";
            node.style.top = `${y}px`;
          }
        }
      }
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  }, []);

  // ── Previous-day reference lines (PDC/PDH/PDL) ─────────────────────────────
  // Horizontal price lines from the previous trading day's daily bar. Reconciled
  // against the requested previous_* indicators by kind.
  const prevDayKindsKey = indicators
    .filter((i) => i.kind === "previous_close" || i.kind === "previous_high" || i.kind === "previous_low")
    .map((i) => i.kind)
    .join(",");
  useEffect(() => {
    const series = candleRef.current;
    if (!series) return;
    const wanted = new Map<keyof typeof PREV_DAY_OPTIONS, number>();
    if (prevDay) {
      for (const ind of indicators) {
        if (ind.kind === "previous_close") wanted.set("previous_close", prevDay.close);
        else if (ind.kind === "previous_high") wanted.set("previous_high", prevDay.high);
        else if (ind.kind === "previous_low") wanted.set("previous_low", prevDay.low);
      }
    }
    // Remove no-longer-wanted lines.
    for (const [id, line] of prevDayLineMap.current) {
      if (!wanted.has(id as keyof typeof PREV_DAY_OPTIONS)) {
        series.removePriceLine(line);
        prevDayLineMap.current.delete(id);
      }
    }
    // Create / update wanted lines.
    for (const [id, price] of wanted) {
      const opt = PREV_DAY_OPTIONS[id];
      const existing = prevDayLineMap.current.get(id);
      if (existing) {
        existing.applyOptions({ price });
      } else {
        prevDayLineMap.current.set(
          id,
          series.createPriceLine({
            price,
            color:            opt.color,
            lineWidth:        1,
            lineStyle:        opt.lineStyle,
            axisLabelVisible: true,
            title:            opt.title,
          }),
        );
      }
    }
  }, [prevDay, prevDayKindsKey]); // eslint-disable-line react-hooks/exhaustive-deps

  // ── Mouse handlers — SL/TP drag only ──────────────────────────────────────
  // Clicks are handled by chart.subscribeDblClick above.
  // These React handlers only manage dragging existing SL/TP lines.

  const onMouseDown = useCallback((e: React.MouseEvent<HTMLDivElement>) => {
    const candle    = candleRef.current;
    const container = containerRef.current;
    if (!candle || !container) return;

    const rect      = container.getBoundingClientRect();
    const y         = e.clientY - rect.top;
    const THRESHOLD = 8;

    const sl = slPriceRef.current;
    const tp = tpPriceRef.current;

    if (sl != null) {
      const slY = candle.priceToCoordinate(sl);
      if (slY != null && Math.abs(y - slY) <= THRESHOLD) {
        dragging.current = "sl";
        e.preventDefault();
        return;
      }
    }
    if (tp != null) {
      const tpY = candle.priceToCoordinate(tp);
      if (tpY != null && Math.abs(y - tpY) <= THRESHOLD) {
        dragging.current = "tp";
        e.preventDefault();
      }
    }
  }, []);

  const onMouseMove = useCallback((e: React.MouseEvent<HTMLDivElement>) => {
    if (!dragging.current) return;
    const candle    = candleRef.current;
    const container = containerRef.current;
    if (!candle || !container) return;

    const rect  = container.getBoundingClientRect();
    const price = candle.coordinateToPrice(e.clientY - rect.top);
    if (price == null) return;

    if (dragging.current === "sl" && slLineRef.current) {
      slLineRef.current.applyOptions({ price });
    } else if (dragging.current === "tp" && tpLineRef.current) {
      tpLineRef.current.applyOptions({ price });
    }
  }, []);

  const onMouseUp = useCallback((e: React.MouseEvent<HTMLDivElement>) => {
    if (!dragging.current) return;
    const candle    = candleRef.current;
    const container = containerRef.current;
    if (!candle || !container) return;

    const rect  = container.getBoundingClientRect();
    const price = candle.coordinateToPrice(e.clientY - rect.top);
    if (price != null) {
      if (dragging.current === "sl") onSlDragEndRef.current(price);
      else                            onTpDragEndRef.current(price);
    }
    dragging.current = null;
  }, []);

  const onMouseLeave = useCallback(() => {
    dragging.current = null;
  }, []);

  // ── Cursor ─────────────────────────────────────────────────────────────────
  const cursor = drawMode === "none" ? "default" : "crosshair";

  // ── Deletable user price lines → ✕ overlay buttons ─────────────────────────
  const deleteTags: { key: string; color: string; title: string; onDelete: () => void }[] = [];
  if (slPrice != null && onDeleteSl) deleteTags.push({ key: "sl", color: "#ef4444", title: "Supprimer le SL", onDelete: onDeleteSl });
  if (tpPrice != null && onDeleteTp) deleteTags.push({ key: "tp", color: "#22c55e", title: "Supprimer le TP", onDelete: onDeleteTp });
  if (onDeleteAlarm) for (const a of alarms) deleteTags.push({ key: `alarm-${a.id}`, color: "#f59e0b", title: "Supprimer l'alarme", onDelete: () => onDeleteAlarm(a.id) });

  return (
    <div className="relative h-full w-full" style={{ cursor }}>
      <div
        ref={containerRef}
        className="h-full w-full"
        onMouseDown={onMouseDown}
        onMouseMove={onMouseMove}
        onMouseUp={onMouseUp}
        onMouseLeave={onMouseLeave}
      />

      {linePoint1 && (
        <div className="pointer-events-none absolute left-2 top-2 rounded bg-amber-900/70 px-1.5 py-0.5 text-[10px] text-amber-300">
          Point 1 sélectionné — cliquez pour le point 2
        </div>
      )}

      {/* ✕ delete buttons for user price lines (SL/TP/alarms). Positioned along
          the right edge; vertical position is set imperatively by the rAF loop. */}
      {deleteTags.map((t) => (
        <button
          key={t.key}
          ref={(el) => {
            if (el) tagBtnRefs.current.set(t.key, el);
            else tagBtnRefs.current.delete(t.key);
          }}
          onClick={t.onDelete}
          title={t.title}
          style={{ right: 56, top: -100, transform: "translateY(-50%)", color: t.color, display: "none" }}
          className="absolute z-20 flex h-3.5 w-3.5 items-center justify-center rounded-sm border border-white/15 bg-black/70 text-[9px] leading-none opacity-70 transition-opacity hover:opacity-100"
        >
          ✕
        </button>
      ))}

      {annotations.map((ann) => {
        const x = (chartRef.current?.timeScale().timeToCoordinate(ann.time as UTCTimestamp) ?? ann.pixelX);
        const y = (candleRef.current?.priceToCoordinate(ann.price) ?? ann.pixelY);
        return (
          <div
            key={ann.id}
            style={{ left: x, top: y, transform: "translate(-50%, -110%)" }}
            className="pointer-events-none absolute rounded bg-amber-900/80 px-1.5 py-0.5 text-[9px] text-amber-300 whitespace-nowrap shadow"
          >
            {ann.text}
          </div>
        );
      })}
    </div>
  );
}
