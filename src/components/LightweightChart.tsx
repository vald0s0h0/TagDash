import { useEffect, useRef, useCallback, useMemo, useState } from "react";
import {
  createChart,
  TickMarkType,
  type IChartApi,
  type ISeriesApi,
  type IPriceLine,
  type UTCTimestamp,
  type CandlestickData,
  type Time,
} from "lightweight-charts";
import { useQuery } from "@tanstack/react-query";
import { api } from "@/lib/tauri";
import type { PaneIndicator, PremarketMode, Timeframe } from "@/types";
import type {
  DrawMode, ChartLine, ChartAnnotation, ChartAlarm, CtxTarget, DrawScope,
} from "@/stores/chartStore";
import type { CrosshairSync } from "@/lib/crosshairSync";
import { nyTime, nyDateTime, nyDayMonth, nyMonth, nyYear } from "@/lib/nyTime";
import { ExecutionsPrimitive } from "@/charts/executionsPrimitive";
import { BollingerPrimitive } from "@/charts/bollingerPrimitive";
import { GridPrimitive } from "@/charts/gridPrimitive";
import { ShortCostPrimitive, type ShortCostBand } from "@/charts/shortCostPrimitive";
import { DipZonePrimitive, type DipZone } from "@/charts/dipZonePrimitive";
import { NewsPrimitive } from "@/charts/newsPrimitive";
import { SessionBandsPrimitive } from "@/charts/sessionBandsPrimitive";
import { PremarketPrimitive } from "@/charts/premarketPrimitive";
import { PremarketChip } from "@/components/PremarketChip";
import { DrawingsPrimitive, hexToRgba } from "@/charts/drawingsPrimitive";
import { useChartTheme, getChartTheme } from "@/stores/chartThemeStore";
import { useChartInput } from "@/stores/chartInputStore";
import {
  BACKFILL_THRESHOLD, toUTC,
  slOpts, tpOpts, alarmOpts, CURSOR_OPTIONS, crosshairOptions,
  spreadLabel, SPREAD_TAG_COLOR,
} from "@/charts/chartOptions";
import { registerChartControl, setHoveredPane, releaseHoveredPane } from "@/lib/gamepadBus";
import { useVolumeSeries } from "@/charts/useVolumeSeries";
import { useReferenceLines } from "@/charts/useReferenceLines";
import { usePrevDayLines } from "@/charts/usePrevDayLines";
import { useIndicators } from "@/charts/useIndicators";
import { useSessionShading } from "@/charts/useSessionShading";
import { useCandleMarkers } from "@/charts/useCandleMarkers";
import { useSlTpLines } from "@/charts/useSlTpLines";
import { useDraftLines } from "@/charts/useDraftLines";
import { useApexLines, type ApexLinesSpec } from "@/charts/useApexLines";
import { useExecutionMarkers } from "@/charts/useExecutionMarkers";
import { useNewsMarkers } from "@/charts/useNewsMarkers";
import { useCrosshairRegister } from "@/charts/useCrosshairRegister";
import { useDeleteButtons } from "@/charts/useDeleteButtons";
import { useBarSeries } from "@/charts/useBarSeries";
import { useLiveTicks } from "@/charts/useLiveTicks";
import { useChartBars } from "@/charts/useChartBars";
import { usePremarketBars } from "@/charts/usePremarketBars";
import { useBarTooltip } from "@/charts/useBarTooltip";
import { useLimitLines, type LimitLineEntry } from "@/charts/useLimitLines";

// ─── Geometry helpers ───────────────────────────────────────────────────────

/** Distance from point (px,py) to segment (ax,ay)-(bx,by), in pixels. */
function pointSegDist(px: number, py: number, ax: number, ay: number, bx: number, by: number): number {
  const dx = bx - ax, dy = by - ay;
  const len2 = dx * dx + dy * dy;
  if (len2 === 0) return Math.hypot(px - ax, py - ay);
  let t = ((px - ax) * dx + (py - ay) * dy) / len2;
  t = Math.max(0, Math.min(1, t));
  return Math.hypot(px - (ax + t * dx), py - (ay + t * dy));
}

// Quantisation of the premarket window's left edge: the visible-range handler is
// per-frame, so only a move of at least this much republishes it.
const PM_RANGE_QUANTUM_SEC = 5 * 86_400;

const HANDLE_HIT = 8;   // px radius to grab an endpoint handle
const SEG_HIT    = 6;   // px distance to grab a segment body
const PRICE_HIT  = 7;   // px to grab a horizontal price line

// Default chart scale options, re-applied after a drag temporarily froze them.
// `mouseWheel` is off: the main-wheel time zoom is driven by our `onWheel` handler
// (which always pins the current/rightmost bar) — native wheel zoom anchors on
// the cursor, which we don't want.
const DEFAULT_SCALE = {
  mouseWheel:           false,
  pinch:                true,
  axisPressedMouseMove: true,
  axisDoubleClickReset: true,
};

// Scroll handling: the chart's OWN mouse-wheel scroll is disabled so the wheel is
// driven entirely by our `onWheel` handler (2nd/horizontal wheel = vertical
// price-axis zoom, main wheel = time zoom). Click-drag panning and touch dragging
// stay enabled. Re-applied after an in-chart drag froze them.
const DEFAULT_SCROLL = {
  mouseWheel:       false,
  pressedMouseMove: true,
  horzTouchDrag:    true,
  vertTouchDrag:    true,
};

// ─── Drag state ───────────────────────────────────────────────────────────────

type DragTarget =
  | { kind: "sl" }
  | { kind: "tp" }
  | { kind: "draft_entry" }
  | { kind: "draft_sl" }
  | { kind: "draft_tp" }
  | { kind: "alarm"; id: string }
  | { kind: "line-p1"; id: string }
  | { kind: "line-p2"; id: string }
  | { kind: "line-body"; id: string; downX: number; downY: number; p1: { time: number; price: number }; p2: { time: number; price: number } }
  | { kind: "ann"; id: string }
  | null;

type DragPreview =
  | { kind: "line"; id: string; point1: { time: number; price: number }; point2: { time: number; price: number } }
  | { kind: "ann"; id: string; time: number; price: number }
  | null;

// ─── Props ────────────────────────────────────────────────────────────────────

interface Props {
  symbol:      string;
  timeframe:   Timeframe;
  drawMode:    DrawMode;
  /** Timeframe class of THIS pane: drawings are created/filtered against it. */
  paneScope:   DrawScope;
  pendingEmoji: string | null;
  /** When set, next chart click places a limit order at that price (% size). */
  pendingLimitPercent?: 25 | 50 | 100 | null;
  /** Called when the user clicks on the chart to finalise a pending limit order. */
  onLimitOrderClick?: (price: number) => void;
  slPrice:     number | null;
  tpPrice:     number | null;
  entryPrice:  number | null;
  shortCostBand?: ShortCostBand | null;
  bid:         number | null;
  ask:         number | null;
  ordersActive: boolean; // true when a position is open → SL/TP shown as orders
  limitOrders?: LimitLineEntry[];
  lines:       ChartLine[];
  annotations: ChartAnnotation[];
  alarms:      ChartAlarm[];
  linePoint1:  { time: number; price: number } | null;
  /** Strategy-card indicators overlaid on this pane (VWAP / EMA / SMA). */
  indicators?: PaneIndicator[];
  /** DAILY panes only: how the 04:00–09:30 premarket session is shown. Ignored on
   *  every intraday pane (their own bars already carry extended hours, and the
   *  session bands already mark them). */
  premarketMode?: PremarketMode;
  /** Cycle that mode. When given (daily panes), the chart renders the PM toggle
   *  itself — it is the only thing that knows how wide the price scale is, and the
   *  chip must sit clear of it. */
  onCyclePremarket?: () => void;
  /** Candle markers (e.g. red dots on split days, HOD/LOD points, series crosses),
   *  unix-seconds keyed. `position`/`shape` default to belowBar/circle. */
  markers?: {
    time: number; color: string; text?: string;
    position?: "aboveBar" | "belowBar" | "inBar";
    shape?: "circle" | "square" | "arrowUp" | "arrowDown";
  }[];
  /** Shared cross-pane crosshair sync group (same instrument). */
  crosshairSync?: CrosshairSync;
  /** Stable id of this pane within its zone, for the crosshair sync registry. */
  paneId?: string;
  // Draft (suggested) trade lines from HOD Drive.
  draftEntry?:    number | null;
  draftSl?:       number | null;
  draftTp?:       number | null;
  onDraftEntryDrag?: (price: number) => void;
  onDraftSlDrag?:    (price: number) => void;
  onDraftTpDrag?:    (price: number) => void;
  /** Armed Apex on this pane's ticker: lines at price ± distance. */
  apex?:          ApexLinesSpec | null;
  /** Dip's band on this pane's ticker: the green zone laid by the trader. */
  dip?:           DipZone | null;
  onPriceClick:   (price: number, time: number, pixelX: number, pixelY: number, scope: DrawScope) => void;
  onSlDragEnd:    (price: number) => void;
  onTpDragEnd:    (price: number) => void;
  /** Double-click shortcut: memorise the clicked price as a (provisional) SL. */
  onSlDblClick:   (price: number) => void;
  /** Delete the user-placed SL / TP / alarm line from its on-chart ✕ button. */
  onDeleteSl?:    () => void;
  onDeleteTp?:    () => void;
  onDeleteAlarm?: (id: string) => void;
  /** Drag an alarm price line to a new price. */
  onAlarmDragEnd?: (id: string, price: number) => void;
  /** Commit a trend-line drag (endpoint or whole segment). */
  onLineChange?:  (id: string, point1: { time: number; price: number }, point2: { time: number; price: number }) => void;
  /** Commit a text/emoji move. */
  onAnnotationMove?: (id: string, time: number, price: number) => void;
  /** Create a text or emoji annotation at (time, price). */
  onCreateAnnotation?: (kind: "text" | "emoji", time: number, price: number, text: string) => void;
  /** Commit a text-annotation edit. */
  onEditAnnotation?: (id: string, text: string) => void;
  /** Right-click on a drawing / price line → open the context menu. */
  onContextMenu?: (target: CtxTarget, clientX: number, clientY: number) => void;
  /** Tool placement was cancelled (e.g. text editor escaped) → reset drawMode. */
  onCancelTool?: () => void;
}

// ─── Component ────────────────────────────────────────────────────────────────

export function LightweightChart({
  symbol,
  timeframe,
  drawMode,
  paneScope,
  pendingEmoji,
  pendingLimitPercent,
  onLimitOrderClick,
  slPrice,
  tpPrice,
  entryPrice,
  shortCostBand = null,
  bid,
  ask,
  ordersActive,
  limitOrders = [],
  lines,
  annotations,
  alarms,
  linePoint1,
  indicators = [],
  premarketMode = "off",
  onCyclePremarket,
  markers = [],
  crosshairSync,
  paneId,
  draftEntry,
  draftSl,
  draftTp,
  onDraftEntryDrag,
  onDraftSlDrag,
  onDraftTpDrag,
  apex = null,
  dip = null,
  onPriceClick,
  onSlDragEnd,
  onTpDragEnd,
  onSlDblClick,
  onDeleteSl,
  onDeleteTp,
  onDeleteAlarm,
  onAlarmDragEnd,
  onLineChange,
  onAnnotationMove,
  onCreateAnnotation,
  onEditAnnotation,
  onContextMenu,
  onCancelTool,
}: Props) {
  const containerRef  = useRef<HTMLDivElement>(null);
  const chartRef      = useRef<IChartApi | null>(null);
  const candleRef     = useRef<ISeriesApi<"Candlestick"> | null>(null);
  const slLineRef     = useRef<IPriceLine | null>(null);
  const tpLineRef     = useRef<IPriceLine | null>(null);
  const entryLineRef  = useRef<IPriceLine | null>(null);
  const bidLineRef    = useRef<IPriceLine | null>(null);
  const askLineRef    = useRef<IPriceLine | null>(null);
  // Draft (suggested) trade lines from HOD Drive.
  const draftEntryLineRef = useRef<IPriceLine | null>(null);
  const draftSlLineRef    = useRef<IPriceLine | null>(null);
  const draftTpLineRef    = useRef<IPriceLine | null>(null);
  const draftEntryPriceRef = useRef<number | null>(draftEntry ?? null);
  const draftSlPriceRef    = useRef<number | null>(draftSl ?? null);
  const draftTpPriceRef    = useRef<number | null>(draftTp ?? null);
  // Controller horizontal cursor (right stick) — a movable price line + its price.
  const cursorLineRef  = useRef<IPriceLine | null>(null);
  const cursorPriceRef = useRef<number | null>(null);
  // User-placed price alarms (amber dashed lines), keyed by alarm id.
  const alarmLineMap  = useRef<Map<string, IPriceLine>>(new Map());
  // Previous-day reference levels (PDC/PDH/PDL), keyed by indicator kind.
  const prevDayLineMap = useRef<Map<string, IPriceLine>>(new Map());
  // Strategy-card indicator series (VWAP / EMA / SMA = Line), keyed by id.
  const indicatorSeriesMap = useRef<Map<string, ISeriesApi<"Line"> | ISeriesApi<"Histogram">>>(new Map());
  const volumeSeriesRef = useRef<ISeriesApi<"Histogram"> | null>(null);
  const sessionPrimRef  = useRef<SessionBandsPrimitive | null>(null);
  const premarketPrimRef = useRef<PremarketPrimitive | null>(null);
  const execPrimRef     = useRef<ExecutionsPrimitive | null>(null);
  const bollingerPrimRef = useRef<BollingerPrimitive | null>(null);
  const gridPrimRef      = useRef<GridPrimitive | null>(null);
  const shortCostPrimRef = useRef<ShortCostPrimitive | null>(null);
  const dipZonePrimRef   = useRef<DipZonePrimitive | null>(null);
  /** Price under the mouse in this pane — read by the cursor-placement hotkeys. */
  const hoverPriceRef    = useRef<number | null>(null);
  const newsPrimRef      = useRef<NewsPrimitive | null>(null);
  // User trend lines + selection handles (custom primitive).
  const drawingsPrimRef = useRef<DrawingsPrimitive | null>(null);
  // Right-edge "price + ✕" label pills for user price lines (SL/TP/alarms).
  const tagBtnRefs      = useRef<Map<string, HTMLElement>>(new Map());
  const tagPriceRef     = useRef<Map<string, number>>(new Map());

  // Always-fresh refs for callbacks and props — avoids stale closures
  const onPriceClickRef = useRef(onPriceClick);
  const onSlDragEndRef  = useRef(onSlDragEnd);
  const onTpDragEndRef  = useRef(onTpDragEnd);
  const onSlDblClickRef = useRef(onSlDblClick);
  const onCreateAnnotationRef = useRef(onCreateAnnotation);
  const drawModeRef     = useRef<DrawMode>(drawMode);
  const paneScopeRef    = useRef<DrawScope>(paneScope);
  const pendingEmojiRef = useRef<string | null>(pendingEmoji);
  const pendingLimitRef = useRef<25 | 50 | 100 | null>(pendingLimitPercent ?? null);
  const onLimitOrderClickRef = useRef(onLimitOrderClick);
  const slPriceRef      = useRef<number | null>(slPrice);
  const tpPriceRef      = useRef<number | null>(tpPrice);
  const onDraftEntryDragRef = useRef(onDraftEntryDrag);
  const onDraftSlDragRef    = useRef(onDraftSlDrag);
  const onDraftTpDragRef    = useRef(onDraftTpDrag);
  const ordersActiveRef = useRef<boolean>(ordersActive);
  const symbolRef       = useRef<string>(symbol);
  const timeframeRef    = useRef<Timeframe>(timeframe);
  const linesRef        = useRef<ChartLine[]>(lines);
  const alarmsRef       = useRef<ChartAlarm[]>(alarms);
  const barsRef         = useRef<Awaited<ReturnType<typeof api.getTickerBars>>>();
  const lastBarRef      = useRef<CandlestickData | null>(null);
  const renderedFirstRef = useRef<number | null>(null);
  const renderedLastRef  = useRef<number | null>(null);

  useEffect(() => { onPriceClickRef.current = onPriceClick; });
  useEffect(() => { onSlDragEndRef.current  = onSlDragEnd; });
  useEffect(() => { onTpDragEndRef.current  = onTpDragEnd; });
  useEffect(() => { onSlDblClickRef.current = onSlDblClick; });
  useEffect(() => { onCreateAnnotationRef.current = onCreateAnnotation; });
  useEffect(() => { drawModeRef.current     = drawMode; });
  useEffect(() => { paneScopeRef.current    = paneScope; });
  useEffect(() => { pendingEmojiRef.current = pendingEmoji; });
  useEffect(() => { pendingLimitRef.current = pendingLimitPercent ?? null; });
  useEffect(() => { onLimitOrderClickRef.current = onLimitOrderClick; });
  useEffect(() => { slPriceRef.current      = slPrice; });
  useEffect(() => { tpPriceRef.current      = tpPrice; });
  useEffect(() => { ordersActiveRef.current = ordersActive; });
  useEffect(() => { draftEntryPriceRef.current = draftEntry ?? null; });
  useEffect(() => { draftSlPriceRef.current    = draftSl ?? null; });
  useEffect(() => { draftTpPriceRef.current    = draftTp ?? null; });
  useEffect(() => { onDraftEntryDragRef.current = onDraftEntryDrag; });
  useEffect(() => { onDraftSlDragRef.current    = onDraftSlDrag; });
  useEffect(() => { onDraftTpDragRef.current    = onDraftTpDrag; });
  useEffect(() => { symbolRef.current       = symbol; });
  useEffect(() => { timeframeRef.current    = timeframe; });
  useEffect(() => { linesRef.current        = lines; });
  useEffect(() => { alarmsRef.current       = alarms; });

  // Selection + drag + inline text editor — pane-local UI state.
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [dragPreview, setDragPreview] = useState<DragPreview>(null);
  // True while a drag is in progress → window-level move/up listeners are bound.
  const [dragActive, setDragActive] = useState(false);
  // Mirror of dragPreview for the commit handler (avoids a stale-closure read on
  // a fast mouseup landing before the move's re-render).
  const dragPreviewRef = useRef<DragPreview>(null);
  const setPreview = (p: DragPreview) => { dragPreviewRef.current = p; setDragPreview(p); };
  const [textEdit, setTextEdit] = useState<
    { x: number; y: number; time: number; price: number; editingId: string | null; value: string } | null
  >(null);
  const dragging = useRef<DragTarget>(null);

  // User-tunable chart appearance (Settings → Apparence). Subscribing here re-runs
  // the styling hooks/effects below when the palette is edited, for a live update.
  const theme = useChartTheme();

  // Wheel / trackpad tuning (Settings → Apparence). Mirrored into refs so the
  // once-bound `onWheel` handler reads live values without re-binding.
  const mouseSensitivityX    = useChartInput((s) => s.mouseSensitivityX);
  const mouseSensitivityY    = useChartInput((s) => s.mouseSensitivityY);
  const trackpadSensitivityX = useChartInput((s) => s.trackpadSensitivityX);
  const trackpadSensitivityY = useChartInput((s) => s.trackpadSensitivityY);
  const zoomInvert           = useChartInput((s) => s.zoomInvert);
  const scrollSwapAxes       = useChartInput((s) => s.scrollSwapAxes);
  const mouseSensXRef    = useRef(mouseSensitivityX);
  const mouseSensYRef    = useRef(mouseSensitivityY);
  const trackpadSensXRef = useRef(trackpadSensitivityX);
  const trackpadSensYRef = useRef(trackpadSensitivityY);
  const zoomInvertRef    = useRef(zoomInvert);
  const scrollSwapRef    = useRef(scrollSwapAxes);
  useEffect(() => { mouseSensXRef.current    = mouseSensitivityX; },    [mouseSensitivityX]);
  useEffect(() => { mouseSensYRef.current    = mouseSensitivityY; },    [mouseSensitivityY]);
  useEffect(() => { trackpadSensXRef.current = trackpadSensitivityX; }, [trackpadSensitivityX]);
  useEffect(() => { trackpadSensYRef.current = trackpadSensitivityY; }, [trackpadSensitivityY]);
  useEffect(() => { zoomInvertRef.current    = zoomInvert; },           [zoomInvert]);
  useEffect(() => { scrollSwapRef.current    = scrollSwapAxes; },       [scrollSwapAxes]);

  // ── Bar-loading pipeline (history refresh + RAM poll + lazy back-fill) — hook ─
  const { bars, loadOlderBars } = useChartBars(
    symbol, timeframe, symbolRef, timeframeRef,
    barsRef, renderedFirstRef, renderedLastRef,
  );

  // ── Previous-day reference levels (PDC/PDH/PDL) ────────────────────────────
  const wantsPrevDay = indicators.some(
    (i) => i.kind === "previous_close" || i.kind === "previous_high" || i.kind === "previous_low",
  );
  const { data: prevDay } = useQuery({
    queryKey: ["prev_day_levels", symbol],
    queryFn:  () => api.getPreviousDayLevels(symbol),
    enabled:  !!symbol && wantsPrevDay,
    staleTime: 5 * 60 * 1000,
  });

  // ── Split-day markers (red dots) — fetched for EVERY daily pane ────────────
  const isDailyTf = timeframe === "daily";
  const { data: splitMarkers } = useQuery({
    queryKey: ["split_markers", symbol],
    queryFn:  () => api.getSplitMarkers(symbol),
    enabled:  !!symbol && isDailyTf,
    staleTime: 6 * 60 * 60 * 1000,
  });
  const allMarkers = useMemo(() => {
    // Keyed by time + position + shape so several markers can share a bar (e.g. a
    // HOD point above and a green-series cross below the same candle).
    type M = NonNullable<typeof markers>[number];
    const byKey = new Map<string, M>();
    const key = (m: M) => `${m.time}:${m.position ?? "belowBar"}:${m.shape ?? "circle"}`;
    if (isDailyTf) for (const m of splitMarkers ?? []) {
      const sm: M = { time: m.time, color: theme.markers.split, text: m.label };
      byKey.set(key(sm), sm);
    }
    for (const m of markers) byKey.set(key(m), m);
    return [...byKey.values()];
  }, [markers, splitMarkers, isDailyTf, theme.markers.split]);

  // ── Trade executions (triangles + P&L line) — extracted hook ───────────────
  useExecutionMarkers(execPrimRef, symbol, bars);

  // ── News pastilles (bottom-of-pane dots) — extracted hook ──────────────────
  useNewsMarkers(newsPrimRef, symbol, bars, timeframe);

  // ── Coordinate helpers (read live each call from the chart) ────────────────
  const timeToX = useCallback((t: number): number | null => {
    return chartRef.current?.timeScale().timeToCoordinate(t as Time) ?? null;
  }, []);
  const priceToY = useCallback((p: number): number | null => {
    return candleRef.current?.priceToCoordinate(p) ?? null;
  }, []);
  const yToPrice = useCallback((y: number): number | null => {
    return candleRef.current?.coordinateToPrice(y) ?? null;
  }, []);
  const xToBarTime = useCallback((x: number): number | null => {
    const t = chartRef.current?.timeScale().coordinateToTime(x);
    return t == null ? null : Number(t);
  }, []);

  // ── Imperative chart controls (wheel + gamepad sticks) ─────────────────────
  // Shared by the mouse `onWheel` handler and the controller's analog sticks (via
  // the gamepad chart-control registry). Defined at component scope (reading refs)
  // so both call sites and the cleanup can reach them.

  // Price-axis zoom: step>0 loosens (zoom out), step<0 tightens (zoom in), by
  // nudging the price-scale margins symmetrically (smaller margins = taller candles).
  const zoomPriceAxis = useCallback((step: number) => {
    const candle = candleRef.current;
    if (!candle) return;
    const scale = candle.priceScale();
    const m = scale.options().scaleMargins ?? { top: 0.08, bottom: 0.08 };
    const clamp = (v: number) => Math.max(0, Math.min(0.45, v));
    scale.applyOptions({ scaleMargins: { top: clamp(m.top + step), bottom: clamp(m.bottom + step) } });
  }, []);

  // Time-axis zoom. factor>1 = zoom in. Always keep the current/rightmost visible
  // bar fixed and grow/shrink the range from the left. This deliberately avoids a
  // cursor anchor so mouse wheels and vertical trackpad scrolling behave alike.
  const zoomTimeAxis = useCallback((factor: number) => {
    const chart = chartRef.current;
    const container = containerRef.current;
    if (!chart || !container) return;
    const ts = chart.timeScale();
    const logical = ts.getVisibleLogicalRange();
    if (!logical) return;
    const n = barsRef.current?.length ?? 0;
    const lastIdx = n - 1;
    const width = container.clientWidth;
    if (n === 0 || logical.to >= lastIdx) {
      // Keep the pixel gap past the last bar fixed while zooming, so the
      // current bar's on-screen position doesn't drift. Derived entirely from
      // the live visible logical range + measured container width, NOT from
      // the timeScale's barSpacing/rightOffset options — those can lag behind
      // a burst of wheel events (worse on HiDPI screens, where each redraw
      // costs ~4x more to rasterize), and reading them back mid-burst made
      // the "preserved" pixel gap balloon every tick instead of staying put.
      const barsVisible = logical.to - logical.from;
      if (barsVisible <= 0) return;
      const spacing = width / barsVisible;
      const gapBars = logical.to - lastIdx;
      const pxOffset = gapBars * spacing;
      const minSpacing = ts.options().minBarSpacing;
      const nextSpacing = Math.max(minSpacing, Math.min(spacing * factor, 80));
      const newBarsVisible = width / nextSpacing;
      if (factor > 1 && newBarsVisible < width / 80) return;
      const to = lastIdx + pxOffset / nextSpacing;
      ts.setVisibleLogicalRange({ from: to - newBarsVisible, to });
      return;
    }
    const inv  = 1 / factor;
    const to   = logical.to;
    const from = to - (to - logical.from) * inv;
    if (factor > 1 && to - from < width / 80) return;
    ts.setVisibleLogicalRange({ from, to });
  }, []);

  const clearCursorLine = useCallback(() => {
    const candle = candleRef.current;
    if (candle && cursorLineRef.current) candle.removePriceLine(cursorLineRef.current);
    cursorLineRef.current  = null;
    cursorPriceRef.current = null;
  }, []);

  // Move the horizontal cursor by a fraction of the visible price span. Seeds at
  // the current price (last bar close) on the first nudge for a fresh symbol.
  const nudgeCursor = useCallback((deltaFrac: number) => {
    const candle = candleRef.current;
    const container = containerRef.current;
    if (!candle || !container) return;
    let price = cursorPriceRef.current;
    if (price == null) {
      const seed = lastBarRef.current?.close;
      if (seed == null) return;
      price = Number(seed);
    }
    const top = candle.coordinateToPrice(0);
    const bot = candle.coordinateToPrice(container.clientHeight);
    const span = top != null && bot != null ? Math.abs(Number(top) - Number(bot)) : price * 0.04;
    const next = price + deltaFrac * span;
    cursorPriceRef.current = next;
    if (cursorLineRef.current) cursorLineRef.current.applyOptions({ price: next });
    else cursorLineRef.current = candle.createPriceLine({ price: next, ...CURSOR_OPTIONS });
  }, []);

  // Register this pane's controls for the gamepad loop; cursor clears on unmount.
  useEffect(() => {
    if (!paneId) return;
    const un = registerChartControl(paneId, {
      zoomTime: zoomTimeAxis,
      zoomPrice: zoomPriceAxis,
      nudgeCursor,
      getCursorPrice: () => cursorPriceRef.current,
      getHoverPrice: () => hoverPriceRef.current,
      clearCursor: clearCursorLine,
    });
    return () => { un(); clearCursorLine(); releaseHoveredPane(paneId); };
  }, [paneId, zoomTimeAxis, zoomPriceAxis, nudgeCursor, clearCursorLine]);

  // Drop the cursor when the symbol changes so it re-seeds at the new price.
  useEffect(() => { clearCursorLine(); }, [symbol, clearCursorLine]);

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
      localization: {
        timeFormatter: (t: Time) =>
          nyDateTime(Number(t), timeframe === "5s" || timeframe === "10s"),
      },
      grid: {
        // Native grid fully off: vertical lines removed; horizontal lines are
        // drawn by GridPrimitive (snapped to round/half-dollar, never < $0.50).
        vertLines: { visible: false },
        horzLines: { visible: false },
      },
      // Colour + "horizontal line only" come from the theme (Settings → Apparence);
      // a dedicated effect re-applies them on every edit (this setup runs once).
      crosshair: crosshairOptions(getChartTheme()),
      timeScale: {
        borderColor:      "#1e1e1e",
        timeVisible:      true,
        secondsVisible:   timeframe === "5s" || timeframe === "10s",
        rightOffset:      7, // bars of empty space past the latest bar (official option)
        fixLeftEdge:      false,
        fixRightEdge:     false,
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
      handleScroll: { ...DEFAULT_SCROLL },
      handleScale: { ...DEFAULT_SCALE },
    });

    // Seed candle colours from the live theme; a dedicated effect re-applies them
    // when the user edits the palette (this setup effect runs once).
    const { up, down } = getChartTheme().candle;
    const candle = chart.addCandlestickSeries({
      upColor:          up,
      downColor:        down,
      borderUpColor:    up,
      borderDownColor:  down,
      wickUpColor:      up,
      wickDownColor:    down,
      priceLineVisible: false,
    });

    chartRef.current  = chart;
    candleRef.current = candle;

    // Pre/post-market zones + day separator. Attached first: it is the backdrop
    // everything else — grid included — is drawn over.
    const sessionPrim = new SessionBandsPrimitive();
    candle.attachPrimitive(sessionPrim);
    sessionPrimRef.current = sessionPrim;

    // Premarket ghost (daily panes). Its fill is a backdrop like the session
    // bands; its high/low caps are a top-z view, so they stay readable over the
    // candle body — see the primitive's own header.
    const premarketPrim = new PremarketPrimitive();
    candle.attachPrimitive(premarketPrim);
    premarketPrimRef.current = premarketPrim;

    // Horizontal grid (round/half-dollar levels). Attached next so it sits
    // behind the other bottom-z primitives (Bollinger fill) and the candles.
    const gridPrim = new GridPrimitive();
    candle.attachPrimitive(gridPrim);
    gridPrimRef.current = gridPrim;

    const execPrim = new ExecutionsPrimitive();
    candle.attachPrimitive(execPrim);
    execPrimRef.current = execPrim;

    // News pastilles — small dots pinned to the pane bottom (over the volume).
    const newsPrim = new NewsPrimitive();
    candle.attachPrimitive(newsPrim);
    newsPrimRef.current = newsPrim;

    const bollingerPrim = new BollingerPrimitive();
    candle.attachPrimitive(bollingerPrim);
    bollingerPrimRef.current = bollingerPrim;

    const shortCostPrim = new ShortCostPrimitive();
    candle.attachPrimitive(shortCostPrim);
    shortCostPrimRef.current = shortCostPrim;

    // Dip's band sits at the very bottom too: the price action that walks into it
    // has to stay readable.
    const dipZonePrim = new DipZonePrimitive();
    candle.attachPrimitive(dipZonePrim);
    dipZonePrimRef.current = dipZonePrim;

    const drawingsPrim = new DrawingsPrimitive();
    candle.attachPrimitive(drawingsPrim);
    drawingsPrimRef.current = drawingsPrim;

    // ── Single click: place the active tool ─────────────────────────────────
    chart.subscribeClick((param) => {
      // Pending limit order: intercept before draw mode so the click places the order.
      if (pendingLimitRef.current != null) {
        if (!param.point) return;
        const price = candle.coordinateToPrice(param.point.y);
        if (price == null || price <= 0) return;
        onLimitOrderClickRef.current?.(price);
        return;
      }

      const mode = drawModeRef.current;
      if (mode === "none") return;          // selection handled in onMouseDown
      if (!param.point) return;

      const price = candle.coordinateToPrice(param.point.y);
      if (price == null) return;
      const time = param.time != null
        ? Number(param.time)
        : (chart.timeScale().coordinateToTime(param.point.x) as number | null) ?? 0;

      if (mode === "sl") {
        if (slLineRef.current) slLineRef.current.applyOptions({ price });
        else slLineRef.current = candle.createPriceLine({ price, ...slOpts(ordersActiveRef.current) });
        slPriceRef.current = price;
        onPriceClickRef.current(price, time, param.point.x, param.point.y, paneScopeRef.current);
      } else if (mode === "tp") {
        let title = "TP";
        const sl   = slPriceRef.current;
        const b    = barsRef.current;
        if (sl != null && b?.length) {
          const entry = b[b.length - 1].close;
          const risk  = Math.abs(entry - sl);
          if (risk > 0) title = `TP  ${(Math.abs(price - entry) / risk).toFixed(1)}R`;
        }
        if (tpLineRef.current) tpLineRef.current.applyOptions({ price, title });
        else tpLineRef.current = candle.createPriceLine({ price, ...tpOpts(ordersActiveRef.current, title) });
        tpPriceRef.current = price;
        onPriceClickRef.current(price, time, param.point.x, param.point.y, paneScopeRef.current);
      } else if (mode === "alarm" || mode === "line") {
        onPriceClickRef.current(price, time, param.point.x, param.point.y, paneScopeRef.current);
      } else if (mode === "text") {
        setTextEdit({ x: param.point.x, y: param.point.y, time, price, editingId: null, value: "" });
      } else if (mode === "emoji") {
        if (pendingEmojiRef.current) onCreateAnnotationRef.current?.("emoji", time, price, pendingEmojiRef.current);
      }
    });

    // ── Double click: SL shortcut ──────────────────────────────────────────
    chart.subscribeDblClick((param) => {
      if (drawModeRef.current !== "none") return;
      if (!param.point) return;
      const price = candle.coordinateToPrice(param.point.y);
      if (price == null) return;
      if (slLineRef.current) slLineRef.current.applyOptions({ price });
      else slLineRef.current = candle.createPriceLine({ price, ...slOpts(ordersActiveRef.current) });
      slPriceRef.current = price;
      onSlDblClickRef.current(price);
    });

    // ── Global crosshair sync (same instrument, sibling panes) ────────────────
    //
    // Only POINTER-driven moves are broadcast. lightweight-charts also re-fires
    // `crosshairMove` on every data update (recalculateAllPanes → updateCrosshair,
    // with no sourceEvent): mirroring those turned each pane's ticks into a
    // broadcast, the sibling answered on its own next tick, and the crosshair
    // ping-ponged between the two panes' snapped bar centres — the vertical line
    // jumping at trade frequency with the mouse dead still, worst in Market Replay
    // where the tape pours in. The price (y) survived because it is re-derived from
    // the same pixel, which is why only the vertical line looked possessed.
    //
    // A leave (no `point`) has no sourceEvent either and must still propagate, or
    // siblings keep a crosshair the user has already walked away from.
    // ── Where the mouse is, for the hotkeys that place a level at the cursor ──
    // Only POINTER-driven moves count: lightweight-charts re-fires this event on
    // every data update (no `sourceEvent`), and a tick must not be read as the
    // trader pointing somewhere. Leaving the pane drops both the price and the
    // hover claim, so a key pressed over the toolbar can never place a band at a
    // price the mouse walked away from.
    chart.subscribeCrosshairMove((param) => {
      if (param.point == null) {
        hoverPriceRef.current = null;
        if (paneId) releaseHoveredPane(paneId);
        return;
      }
      if (param.sourceEvent == null) return;
      hoverPriceRef.current = candle.coordinateToPrice(param.point.y) ?? null;
      if (paneId) setHoveredPane(paneId);
    });

    if (crosshairSync && paneId) {
      chart.subscribeCrosshairMove((param) => {
        if (crosshairSync.syncing) return;
        const leaving = param.point == null;
        if (!leaving && param.sourceEvent == null) return; // data-driven echo
        const price = param.point ? candle.coordinateToPrice(param.point.y) : null;
        crosshairSync.broadcast(
          paneId,
          symbolRef.current,
          param.time != null && price != null ? param.time : null,
          price ?? null,
        );
      });
    }

    chart.timeScale().subscribeVisibleLogicalRangeChange((range) => {
      if (range && range.from < BACKFILL_THRESHOLD) void loadOlderBars();
    });

    const onWheel = (e: WheelEvent) => {
      const absX = Math.abs(e.deltaX);
      const absY = Math.abs(e.deltaY);
      if (absX === 0 && absY === 0) return;

      // Determine dominant scroll axis, then apply the swap-axes preference.
      // Default: horizontal dominant → price-axis zoom, vertical dominant → time-axis zoom.
      // swapAxes: flip that mapping (vertical → price, horizontal → time).
      const hDominant = absX > absY;
      const zoomPrice = scrollSwapRef.current ? !hDominant : hDominant;
      const delta = hDominant ? e.deltaX : e.deltaY;
      if (delta === 0) return;
      e.preventDefault();

      // Mouse-wheel notches produce large deltas (~100 px); trackpad frames are
      // tiny. Sensitivity is picked by (device × physical wheel axis) — not by
      // what that wheel controls — so it stays correct under scrollSwapAxes.
      const absD  = Math.abs(delta);
      const isMouse = absD >= 50;
      const sens  = hDominant
        ? (isMouse ? mouseSensXRef.current : trackpadSensXRef.current)
        : (isMouse ? mouseSensYRef.current : trackpadSensYRef.current);

      if (zoomPrice) {
        const step = Math.min(absD * 0.0002 * sens, 0.05);
        zoomPriceAxis(delta < 0 ? -step : step);
        return;
      }

      // Time-axis (X) zoom.
      const mag   = Math.min(absD * 0.001 * sens, 0.5);
      let zoomIn  = delta < 0;
      if (zoomInvertRef.current) zoomIn = !zoomIn;
      const factor = zoomIn ? 1 + mag : 1 / (1 + mag);
      zoomTimeAxis(factor);
    };
    container.addEventListener("wheel", onWheel, { passive: false });

    const ro = new ResizeObserver(() => {
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
      cursorLineRef.current  = null;
      cursorPriceRef.current = null;
      indicatorSeriesMap.current.clear();
      volumeSeriesRef.current = null;
      sessionPrimRef.current = null;
      execPrimRef.current = null;
      bollingerPrimRef.current = null;
      shortCostPrimRef.current = null;
      dipZonePrimRef.current = null;
      hoverPriceRef.current = null;
      gridPrimRef.current = null;
      premarketPrimRef.current = null;
      newsPrimRef.current = null;
      drawingsPrimRef.current = null;
      alarmLineMap.current.clear();
      prevDayLineMap.current.clear();
    };
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  // ── Register this pane in the crosshair sync group (extracted hook) ─────────
  useCrosshairRegister(chartRef, candleRef, crosshairSync, paneId, symbol);

  // ── Premarket ghost series (daily panes only) ──────────────────────────────
  // The window follows the loaded daily history, so scrolling back extends both
  // together. `enabled` false on every intraday pane and whenever the mode is off,
  // so a pane that isn't showing the ghost costs nothing.
  const pmEnabled = timeframe === "daily" && premarketMode !== "off";

  // Leftmost visible bar time — what sizes the premarket window. NOT `bars[0]`:
  // a daily pane loads 600 bars (~2.5 years) while showing ~100, so asking for the
  // loaded span fired a dozen provider chunks before the first ghost could appear.
  const [visibleFromSec, setVisibleFromSec] = useState<number | null>(null);
  const pmPublishedFromRef = useRef<number | null>(null);
  useEffect(() => {
    const chart = chartRef.current;
    if (!chart || !pmEnabled) return;
    const ts = chart.timeScale();
    pmPublishedFromRef.current = null;
    const publish = (from: number) => {
      const prev = pmPublishedFromRef.current;
      // This handler fires on EVERY frame of a pan, and a React re-render of this
      // component per frame is not affordable. Publish only a meaningfully earlier
      // left edge: panning right or zooming in never re-renders, and a long
      // scroll-back costs a handful of updates instead of hundreds.
      if (prev != null && from > prev - PM_RANGE_QUANTUM_SEC) return;
      pmPublishedFromRef.current = from;
      setVisibleFromSec(from);
    };
    const onRange = (r: { from: Time; to: Time } | null) => {
      if (r?.from != null) publish(Number(r.from));
    };
    // The subscription only fires on CHANGE, so seed from the range as it stands —
    // otherwise switching the ghost on without touching the chart never sizes it.
    // Null while the series is still empty; the first `setData` then fires onRange.
    const cur = ts.getVisibleRange();
    if (cur?.from != null) publish(Number(cur.from));
    ts.subscribeVisibleTimeRangeChange(onRange);
    return () => ts.unsubscribeVisibleTimeRangeChange(onRange);
  }, [pmEnabled, symbol]);

  const premarketBars = usePremarketBars(symbol, pmEnabled, visibleFromSec);

  // Width of the right price scale, so the PM chip can sit just LEFT of it instead
  // of on top of it. It is not a constant: lightweight-charts sizes the scale from
  // the widest price label, so a $0.0819 ticker and a $340 one get different axes.
  const [rightScaleW, setRightScaleW] = useState(0);
  const readScaleWidth = useCallback(() => {
    const chart = chartRef.current;
    if (!chart) return;
    try { setRightScaleW(Math.round(chart.priceScale("right").width())); } catch { /* not laid out yet */ }
  }, []);
  // Daily panes only: premarket is a session INSIDE a daily bar, so the toggle is
  // meaningless on an intraday pane (whose bars already carry extended hours, and
  // whose session bands already mark them).
  // Depend on a stable boolean, never on the callback itself: ChartZone passes an
  // inline arrow and re-renders on its 300 ms polls, which would tear down and
  // rebuild this observer several times a second.
  const showPmChip = !!onCyclePremarket && timeframe === "daily";
  useEffect(() => {
    const container = containerRef.current;
    if (!container || !showPmChip) return;
    readScaleWidth();
    const ro = new ResizeObserver(readScaleWidth);
    ro.observe(container);
    return () => ro.disconnect();
  }, [showPmChip, readScaleWidth]);
  // Label widths also change when the PRICE does (9.99 → 10.00), which no resize
  // event reports. Re-reading on each data render covers it for free: when the bars
  // stop moving, so does the axis. `setState` with an unchanged number is a no-op.
  useEffect(readScaleWidth, [bars, readScaleWidth]);

  // "only" mode: the premarket series feeds the candle series — not to be drawn as
  // candles (they are made transparent below) but so the price scale autoscales to
  // the premarket range and the crosshair reads premarket values. The premarket
  // bars themselves are drawn by the primitive, identically in both modes.
  // Everything else (indicators, SL/TP R-ratios, the click-to-price bar reference)
  // keeps reading `bars`: a Bollinger band or an R multiple computed off premarket
  // candles would be a different statistic wearing the same name.
  const showingPremarket = premarketMode === "only" && !!premarketBars?.length;
  const displayBars = showingPremarket ? premarketBars : bars;

  // A mode flip swaps the series under the SAME timestamps, which `useBarSeries`
  // would read as a pure tail change and render with a single `update()` — leaving
  // every earlier candle showing the old session. Clearing the render bookkeeping
  // here (during render, before that hook's effect) forces a full `setData`.
  const prevShowingPmRef = useRef(showingPremarket);
  if (prevShowingPmRef.current !== showingPremarket) {
    prevShowingPmRef.current = showingPremarket;
    renderedFirstRef.current = null;
    renderedLastRef.current  = null;
  }

  // ── Feed bar data (official lightweight-charts protocol) — extracted hook ──
  useBarSeries(candleRef, displayBars, renderedFirstRef, renderedLastRef, lastBarRef);

  // ── Paint the premarket bars (behind the candles, both modes) ──────────────
  // Depend on the premarket ARRAY, not on `bars`: it only changes when its (5 s,
  // premarket-only) query returns, whereas `bars` gets a new identity on every RAM
  // poll — which would re-map and re-draw every bar once a second for nothing.
  const ghost = premarketMode === "off" ? undefined : premarketBars;
  useEffect(() => {
    const prim = premarketPrimRef.current;
    if (!prim) return;
    prim.setData(
      (ghost ?? []).map((b) => ({
        time:  toUTC(b.time) as number,
        open:  b.open,
        high:  b.high,
        low:   b.low,
        close: b.close,
      })),
    );
  }, [ghost]);


  // ── Live ticks (focus symbols) — update the forming candle (extracted hook) ─
  useLiveTicks(candleRef, lastBarRef, symbol, timeframe);

  // ── Indicators (strategy-card driven: VWAP/EMA/SMA/Bollinger) — extracted hook ─
  useIndicators(chartRef, indicatorSeriesMap, bollingerPrimRef, bars, indicators, theme);

  // ── Volume histogram — always on, every pane (extracted hook) ──────────────
  useVolumeSeries(chartRef, volumeSeriesRef, displayBars, theme);

  // ── Press-and-hold tooltip (bar volume + body % above the bar) — extracted hook ─
  useBarTooltip(chartRef, candleRef, volumeSeriesRef, containerRef);

  // ── Pre/post-market background shading (extracted hook) ────────────────────
  useSessionShading(sessionPrimRef, bars, timeframe);

  // ── Candle markers (e.g. red dots on split days) — extracted hook ──────────
  useCandleMarkers(candleRef, allMarkers, displayBars);

  // ── User SL / TP lines (+ R-ratio title, planned↔order styling) — extracted hook ─
  useSlTpLines(candleRef, slLineRef, tpLineRef, tpPriceRef, slPrice, tpPrice, ordersActive, bars);

  // ── Draft (suggested) trade lines from HOD Drive ──────────────────────────
  useDraftLines(candleRef, draftEntryLineRef, draftSlLineRef, draftTpLineRef,
    draftEntry ?? null, draftSl ?? null, draftTp ?? null);

  // ── Armed Apex lines (price ± ATR distance, follow the ticks) ──────────────
  useApexLines(candleRef, lastBarRef, symbol, timeframe === "daily" ? null : apex);

  // ── Pending limit order lines (dotted, light) ─────────────────────────────
  useLimitLines(candleRef, limitOrders);

  // ── Reference price lines — entry + live bid/ask (extracted hook) ──────────
  useReferenceLines(candleRef, entryLineRef, bidLineRef, askLineRef, entryPrice, bid, ask);

  // Massive Starter has no historical NBBO. When quotes are absent, show the
  // candlestick series' native last-price line instead of two fabricated bid/ask
  // lines. It follows each replay tick automatically, including 1-second data.
  useEffect(() => {
    candleRef.current?.applyOptions({
      priceLineVisible: bid == null && ask == null,
      lastValueVisible: bid == null && ask == null,
    });
  }, [bid, ask]);

  // ── Locate/borrow break-even loss band ───────────────────────────────────
  useEffect(() => {
    shortCostPrimRef.current?.setData(shortCostBand);
  }, [shortCostBand]);

  // ── Dip's band (green zone, behind the candles) ──────────────────────────
  useEffect(() => {
    dipZonePrimRef.current?.setData(dip);
  }, [dip]);

  // ── User trend lines (custom primitive) — fed with live drag preview ───────
  const linesKey = lines.map((l) => `${l.id}:${l.point1.time},${l.point1.price},${l.point2.time},${l.point2.price}:${l.color}:${l.opacity}:${l.width}:${l.lineStyle}`).join("|");
  useEffect(() => {
    const display = lines.map((l) =>
      dragPreview?.kind === "line" && dragPreview.id === l.id
        ? { ...l, point1: dragPreview.point1, point2: dragPreview.point2 }
        : l,
    );
    drawingsPrimRef.current?.setData(display, selectedId);
  }, [linesKey, selectedId, dragPreview]); // eslint-disable-line react-hooks/exhaustive-deps

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
      if (existing) existing.applyOptions({ price: a.price });
      else alarmLineMap.current.set(a.id, series.createPriceLine({ price: a.price, ...alarmOpts() }));
    }
  }, [alarmsKey]); // eslint-disable-line react-hooks/exhaustive-deps

  // ── Right-edge label pills: ✕ on user lines (SL/TP/alarms), spread on the bid ─
  // The spread only exists with both quotes, and it rides the bid line — the ask
  // keeps its own native price tag.
  const spread = bid != null && ask != null ? spreadLabel(bid, ask) : null;
  useDeleteButtons(
    candleRef, tagBtnRefs, tagPriceRef, slPrice, tpPrice, alarms,
    spread != null ? bid : null,
    onDeleteSl, onDeleteTp, onDeleteAlarm,
  );

  // ── Previous-day reference lines (PDC/PDH/PDL) — extracted hook ────────────
  usePrevDayLines(candleRef, prevDayLineMap, indicators, prevDay);

  // ── Live theme apply ───────────────────────────────────────────────────────
  // Re-style the once-created candle series + reconciled price lines, and poke the
  // theme-reading primitives (grid / executions / session bands) to redraw, and
  // re-apply the crosshair options, whenever the palette is edited. The data hooks
  // above (volume / indicators / Bollinger / split markers) re-run on `theme`
  // themselves, so they're not handled here.
  useEffect(() => {
    // Premarket-only mode: the series still carries the premarket bars (for the
    // autoscale and the crosshair) but draws nothing — the primitive owns that
    // look, so the two must not be painted on top of each other.
    const CLEAR = "rgba(0,0,0,0)";
    const { up, down } = showingPremarket
      ? { up: CLEAR, down: CLEAR }
      : theme.candle;
    candleRef.current?.applyOptions({
      upColor: up, downColor: down,
      borderUpColor: up, borderDownColor: down,
      wickUpColor: up, wickDownColor: down,
    });
    slLineRef.current?.applyOptions({ color: slOpts(ordersActiveRef.current).color });
    tpLineRef.current?.applyOptions({ color: tpOpts(ordersActiveRef.current).color });
    for (const line of alarmLineMap.current.values()) line.applyOptions({ color: theme.levels.alarm });
    chartRef.current?.applyOptions({ crosshair: crosshairOptions(theme) });
    gridPrimRef.current?.redraw();
    execPrimRef.current?.redraw();
    newsPrimRef.current?.redraw();
    sessionPrimRef.current?.redraw();
    premarketPrimRef.current?.redraw();
  }, [theme, showingPremarket]);

  // ── Freeze / restore the chart's pan+zoom around an in-chart drag ──────────
  const freezePan = useCallback((freeze: boolean) => {
    chartRef.current?.applyOptions(
      freeze
        ? { handleScroll: false, handleScale: false }
        : { handleScroll: { ...DEFAULT_SCROLL }, handleScale: { ...DEFAULT_SCALE } },
    );
  }, []);

  // ── Hit-test which draggable object is under the cursor (container coords) ──
  const hitTest = useCallback((x: number, y: number): DragTarget => {
    // 1. Price lines (SL / TP / drafts / alarms).
    const sl = slPriceRef.current, tp = tpPriceRef.current;
    if (sl != null) { const yy = priceToY(sl); if (yy != null && Math.abs(y - yy) <= PRICE_HIT) return { kind: "sl" }; }
    if (tp != null) { const yy = priceToY(tp); if (yy != null && Math.abs(y - yy) <= PRICE_HIT) return { kind: "tp" }; }
    const de = draftEntryPriceRef.current, ds = draftSlPriceRef.current, dt = draftTpPriceRef.current;
    if (de != null) { const yy = priceToY(de); if (yy != null && Math.abs(y - yy) <= PRICE_HIT) return { kind: "draft_entry" }; }
    if (ds != null) { const yy = priceToY(ds); if (yy != null && Math.abs(y - yy) <= PRICE_HIT) return { kind: "draft_sl" }; }
    if (dt != null) { const yy = priceToY(dt); if (yy != null && Math.abs(y - yy) <= PRICE_HIT) return { kind: "draft_tp" }; }
    for (const a of alarmsRef.current) {
      const yy = priceToY(a.price);
      if (yy != null && Math.abs(y - yy) <= PRICE_HIT) return { kind: "alarm", id: a.id };
    }
    // 2./3. Trend-line endpoints, then segment bodies.
    for (const l of linesRef.current) {
      const ax = timeToX(l.point1.time), ay = priceToY(l.point1.price);
      const bx = timeToX(l.point2.time), by = priceToY(l.point2.price);
      if (ax == null || ay == null || bx == null || by == null) continue;
      if (Math.hypot(x - ax, y - ay) <= HANDLE_HIT) return { kind: "line-p1", id: l.id };
      if (Math.hypot(x - bx, y - by) <= HANDLE_HIT) return { kind: "line-p2", id: l.id };
    }
    for (const l of linesRef.current) {
      const ax = timeToX(l.point1.time), ay = priceToY(l.point1.price);
      const bx = timeToX(l.point2.time), by = priceToY(l.point2.price);
      if (ax == null || ay == null || bx == null || by == null) continue;
      if (pointSegDist(x, y, ax, ay, bx, by) <= SEG_HIT)
        return { kind: "line-body", id: l.id, downX: x, downY: y, p1: { ...l.point1 }, p2: { ...l.point2 } };
    }
    return null;
  }, [timeToX, priceToY]);

  // ── Mouse handlers ─────────────────────────────────────────────────────────
  const onMouseDown = useCallback((e: React.MouseEvent<HTMLDivElement>) => {
    if (drawModeRef.current !== "none") return; // tool placement goes via click
    if (e.button !== 0) return;
    const container = containerRef.current;
    if (!container) return;
    const rect = container.getBoundingClientRect();
    const x = e.clientX - rect.left;
    const y = e.clientY - rect.top;

    const hit = hitTest(x, y);
    if (!hit) { setSelectedId(null); return; }

    if (hit.kind === "line-p1" || hit.kind === "line-p2" || hit.kind === "line-body") {
      setSelectedId(hit.id);
    }
    dragging.current = hit;
    freezePan(true);
    setDragActive(true);
    e.preventDefault();
  }, [hitTest, freezePan]);

  // Native (window-level) move handler — see the drag useEffect below.
  const dragMove = useCallback((e: MouseEvent) => {
    const drag = dragging.current;
    if (!drag) return;
    const container = containerRef.current;
    if (!container) return;
    const rect = container.getBoundingClientRect();
    const x = e.clientX - rect.left;
    const y = e.clientY - rect.top;
    const price = yToPrice(y);
    if (price == null) return;

    if (drag.kind === "sl" && slLineRef.current) {
      slLineRef.current.applyOptions({ price }); slPriceRef.current = price;
    } else if (drag.kind === "tp" && tpLineRef.current) {
      tpLineRef.current.applyOptions({ price }); tpPriceRef.current = price;
    } else if (drag.kind === "draft_entry" && draftEntryLineRef.current) {
      draftEntryLineRef.current.applyOptions({ price }); draftEntryPriceRef.current = price;
    } else if (drag.kind === "draft_sl" && draftSlLineRef.current) {
      draftSlLineRef.current.applyOptions({ price }); draftSlPriceRef.current = price;
    } else if (drag.kind === "draft_tp" && draftTpLineRef.current) {
      draftTpLineRef.current.applyOptions({ price }); draftTpPriceRef.current = price;
    } else if (drag.kind === "alarm") {
      alarmLineMap.current.get(drag.id)?.applyOptions({ price });
    } else if (drag.kind === "line-p1" || drag.kind === "line-p2") {
      const l = linesRef.current.find((ln) => ln.id === drag.id);
      if (!l) return;
      const t = xToBarTime(x) ?? (drag.kind === "line-p1" ? l.point1.time : l.point2.time);
      const np = { time: t, price };
      setPreview({ kind: "line", id: l.id,
        point1: drag.kind === "line-p1" ? np : l.point1,
        point2: drag.kind === "line-p2" ? np : l.point2 });
    } else if (drag.kind === "line-body") {
      const dPrice = price - (yToPrice(drag.downY) ?? price);
      const moveEnd = (pt: { time: number; price: number }) => {
        const px = timeToX(pt.time);
        const nt = px == null ? pt.time : (xToBarTime(px + (x - drag.downX)) ?? pt.time);
        return { time: nt, price: pt.price + dPrice };
      };
      setPreview({ kind: "line", id: drag.id, point1: moveEnd(drag.p1), point2: moveEnd(drag.p2) });
    } else if (drag.kind === "ann") {
      const t = xToBarTime(x);
      if (t != null) setPreview({ kind: "ann", id: drag.id, time: t, price });
    }
  }, [yToPrice, xToBarTime, timeToX]);

  const commitDrag = useCallback(() => {
    const drag = dragging.current;
    dragging.current = null;
    if (!drag) return;
    freezePan(false);

    const preview = dragPreviewRef.current;
    if (drag.kind === "sl") { if (slPriceRef.current != null) onSlDragEndRef.current(slPriceRef.current); }
    else if (drag.kind === "tp") { if (tpPriceRef.current != null) onTpDragEndRef.current(tpPriceRef.current); }
    else if (drag.kind === "draft_entry") { if (draftEntryPriceRef.current != null) onDraftEntryDragRef.current?.(draftEntryPriceRef.current); }
    else if (drag.kind === "draft_sl") { if (draftSlPriceRef.current != null) onDraftSlDragRef.current?.(draftSlPriceRef.current); }
    else if (drag.kind === "draft_tp") { if (draftTpPriceRef.current != null) onDraftTpDragRef.current?.(draftTpPriceRef.current); }
    else if (drag.kind === "alarm") {
      const ln = alarmLineMap.current.get(drag.id);
      const price = (ln?.options() as { price?: number } | undefined)?.price;
      if (price != null) onAlarmDragEnd?.(drag.id, price);
    } else if (preview?.kind === "line") {
      onLineChange?.(preview.id, preview.point1, preview.point2);
    } else if (preview?.kind === "ann") {
      onAnnotationMove?.(preview.id, preview.time, preview.price);
    }
    setPreview(null);
  }, [freezePan, onAlarmDragEnd, onLineChange, onAnnotationMove]);

  // ── Window-level drag listeners ─────────────────────────────────────────────
  // Bind move/up to the window (not the chart container) so a drag keeps tracking
  // even when the cursor leaves the container or hovers the annotation overlay
  // (which sits on top and follows the cursor). Otherwise text/emoji objects are
  // "lost" mid-drag.
  useEffect(() => {
    if (!dragActive) return;
    const onMove = (e: MouseEvent) => dragMove(e);
    const onUp = () => { if (dragging.current) commitDrag(); setDragActive(false); };
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
    return () => {
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
    };
  }, [dragActive, dragMove, commitDrag]);

  // Right-click → context menu for a drawing / price line under the cursor.
  const onContextMenuHandler = useCallback((e: React.MouseEvent<HTMLDivElement>) => {
    const container = containerRef.current;
    if (!container || !onContextMenu) return;
    const rect = container.getBoundingClientRect();
    const hit = hitTest(e.clientX - rect.left, e.clientY - rect.top);
    if (!hit) return;
    e.preventDefault();
    let target: CtxTarget;
    if (hit.kind === "sl") target = { type: "sl" };
    else if (hit.kind === "tp") target = { type: "tp" };
    else if (hit.kind === "alarm") target = { type: "alarm", id: hit.id };
    else if (hit.kind === "draft_entry" || hit.kind === "draft_sl" || hit.kind === "draft_tp") return;
    else { setSelectedId(hit.id); target = { type: "line", id: hit.id }; }
    onContextMenu(target, e.clientX, e.clientY);
  }, [hitTest, onContextMenu]);

  // ── Annotation drag (from the overlay element) ──────────────────────────────
  const startAnnDrag = useCallback((e: React.MouseEvent, id: string) => {
    if (drawModeRef.current !== "none" || e.button !== 0) return;
    e.stopPropagation();
    e.preventDefault();
    dragging.current = { kind: "ann", id };
    freezePan(true);
    setDragActive(true);
  }, [freezePan]);

  // ── Cursor ─────────────────────────────────────────────────────────────────
  const cursor = pendingLimitPercent ? "crosshair" : drawMode === "none" ? "default" : "crosshair";

  // ── Deletable user price lines → right-edge "price + ✕" label pills ─────────
  const fmtPrice = (p: number) => (p >= 1 ? p.toFixed(2) : p.toFixed(4));
  const deleteTags: { key: string; price: number; color: string; title: string; onDelete: () => void }[] = [];
  if (slPrice != null && onDeleteSl) deleteTags.push({ key: "sl", price: slPrice, color: "#ef4444", title: "Supprimer le SL", onDelete: onDeleteSl });
  if (tpPrice != null && onDeleteTp) deleteTags.push({ key: "tp", price: tpPrice, color: "#22c55e", title: "Supprimer le TP", onDelete: onDeleteTp });
  if (onDeleteAlarm) for (const a of alarms) deleteTags.push({ key: `alarm-${a.id}`, price: a.price, color: "#f59e0b", title: "Supprimer l'alarme", onDelete: () => onDeleteAlarm(a.id) });

  return (
    <div className="relative h-full w-full" style={{ cursor }}>
      <div
        ref={containerRef}
        className="h-full w-full"
        onMouseDown={onMouseDown}
        onContextMenu={onContextMenuHandler}
      />

      {showPmChip && onCyclePremarket && (
        <div className="absolute z-20" style={{ top: 4, right: rightScaleW + 4 }}>
          <PremarketChip mode={premarketMode} onCycle={onCyclePremarket} />
        </div>
      )}

      {linePoint1 && (
        <div className="pointer-events-none absolute left-2 top-2 rounded bg-amber-900/70 px-1.5 py-0.5 text-[10px] text-amber-300">
          Point 1 sélectionné — cliquez pour le point 2
        </div>
      )}

      {/* Spread pill on the bid line — where the bid's own price tag would be, since
          the pair is read as "ask price / spread", not as two prices. Keyed "spread"
          so the pill-tracking rAF loop parks it on the bid. */}
      {spread != null && (
        <div
          ref={(el) => {
            if (el) tagBtnRefs.current.set("spread", el);
            else tagBtnRefs.current.delete("spread");
          }}
          title="Spread (ask − bid)"
          style={{
            right: 2, top: -100, transform: "translateY(-50%)",
            borderColor: "rgba(200,200,200,0.35)", color: SPREAD_TAG_COLOR,
            display: "none",
          }}
          className="pointer-events-none absolute z-20 flex items-center rounded-sm border bg-black/80 px-1 py-px text-[9px] leading-none tabular-nums"
        >
          {spread}
        </div>
      )}

      {/* "price + ✕" label pills for user price lines (SL/TP/alarms). */}
      {deleteTags.map((t) => (
        <div
          key={t.key}
          ref={(el) => {
            if (el) tagBtnRefs.current.set(t.key, el);
            else tagBtnRefs.current.delete(t.key);
          }}
          style={{ right: 2, top: -100, transform: "translateY(-50%)", borderColor: `${t.color}66`, display: "none" }}
          className="absolute z-20 flex items-center gap-1 rounded-sm border bg-black/80 px-1 py-px text-[9px] leading-none tabular-nums"
        >
          <span style={{ color: t.color }}>{fmtPrice(t.price)}</span>
          <button
            onClick={t.onDelete}
            title={t.title}
            style={{ color: t.color }}
            className="opacity-70 transition-opacity hover:opacity-100"
          >
            ✕
          </button>
        </div>
      ))}

      {/* Text / emoji annotations — draggable, styled, right-click editable. */}
      {annotations.map((ann) => {
        const isDragging = dragPreview?.kind === "ann" && dragPreview.id === ann.id;
        const t = isDragging ? dragPreview.time : ann.time;
        const p = isDragging ? dragPreview.price : ann.price;
        const x = chartRef.current?.timeScale().timeToCoordinate(t as UTCTimestamp) ?? ann.pixelX;
        const y = candleRef.current?.priceToCoordinate(p) ?? ann.pixelY;
        const isEmoji = ann.kind === "emoji";
        return (
          <div
            key={ann.id}
            onMouseDown={(e) => startAnnDrag(e, ann.id)}
            onDoubleClick={(e) => {
              if (isEmoji) return;
              e.stopPropagation();
              setTextEdit({ x: Number(x), y: Number(y), time: ann.time, price: ann.price, editingId: ann.id, value: ann.text });
            }}
            onContextMenu={(e) => {
              e.preventDefault(); e.stopPropagation();
              onContextMenu?.({ type: "annotation", id: ann.id }, e.clientX, e.clientY);
            }}
            style={{
              left: x, top: y,
              transform: "translate(-50%, -110%)",
              color: hexToRgba(ann.color, ann.opacity),
              fontSize: isEmoji ? ann.fontSize : Math.max(9, ann.fontSize),
              cursor: "move",
            }}
            className={isEmoji
              ? "absolute z-10 select-none leading-none"
              : "absolute z-10 select-none whitespace-pre-line rounded bg-black/40 px-1.5 py-0.5 text-center leading-tight shadow"}
          >
            {ann.text}
          </div>
        );
      })}

      {/* Inline text editor (create new text or edit an existing annotation). */}
      {textEdit && (
        <div
          className="absolute z-30"
          style={{ left: textEdit.x, top: textEdit.y, transform: "translate(-50%, -110%)" }}
        >
          <div className="flex items-start gap-1 rounded border border-amber-700/60 bg-zinc-900 px-1.5 py-1 shadow-lg">
            <textarea
              autoFocus
              rows={Math.max(1, textEdit.value.split("\n").length)}
              value={textEdit.value}
              onChange={(e) => setTextEdit({ ...textEdit, value: e.target.value })}
              onKeyDown={(e) => {
                if (e.key === "Enter" && !e.shiftKey) {
                  e.preventDefault();
                  if (!textEdit.value.trim()) return;
                  if (textEdit.editingId) onEditAnnotation?.(textEdit.editingId, textEdit.value.trim());
                  else onCreateAnnotation?.("text", textEdit.time, textEdit.price, textEdit.value.trim());
                  setTextEdit(null);
                } else if (e.key === "Escape") {
                  setTextEdit(null);
                  if (!textEdit.editingId) onCancelTool?.();
                }
              }}
              placeholder="Annotation…"
              className="w-28 resize-none bg-transparent text-[10px] text-amber-300 placeholder-muted-foreground/40 outline-none"
            />
            <button
              onClick={() => {
                if (!textEdit.value.trim()) return;
                if (textEdit.editingId) onEditAnnotation?.(textEdit.editingId, textEdit.value.trim());
                else onCreateAnnotation?.("text", textEdit.time, textEdit.price, textEdit.value.trim());
                setTextEdit(null);
              }}
              className="text-[10px] text-amber-400 hover:text-amber-200"
            >
              ✓
            </button>
          </div>
        </div>
      )}
    </div>
  );
}
