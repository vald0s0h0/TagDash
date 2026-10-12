// TagDash's chart defaults — ONE definition shared by every chart of the app: the
// trading panes (LightweightChart) and the « Revue » replay (main chart, speed of
// tape, daily and 5-minute context). Options, candle/volume styling from the theme,
// pre/post-market zones, strategy-card indicators, and the wheel/trackpad zoom
// (main wheel = time zoom pinned on the latest bar, horizontal wheel = price zoom,
// sensitivities from Settings ▸ Apparence).

import {
  TickMarkType,
  type ChartOptions,
  type DeepPartial,
  type IChartApi,
  type ISeriesApi,
  type LineData,
  type SeriesType,
  type Time,
} from "lightweight-charts";
import type { Bar, PaneIndicator, PaneSpec, StrategyCard, Timeframe } from "@/types";
import { nyDateTime, nyDayMonth, nyDayStamp, nyMonth, nySessionOfMinutes, nyTime, nyYear } from "@/lib/nyTime";
import { crosshairOptions, toUTC } from "@/charts/chartOptions";
import { hexToRgba } from "@/charts/drawingsPrimitive";
import {
  indicatorId, computeEma, computeSma, computeBollinger, computeDailyVwap, BOLLINGER_K,
} from "@/charts/indicators";
import type { BollingerBand, BollingerPrimitive } from "@/charts/bollingerPrimitive";
import type { SessionMark } from "@/charts/sessionBandsPrimitive";
import { getChartTheme, type ChartTheme } from "@/stores/chartThemeStore";
import { useChartInput } from "@/stores/chartInputStore";

// ─── Chart options ──────────────────────────────────────────────────────────────

// Default chart scale options, re-applied after a drag temporarily froze them.
// `mouseWheel` is off: the main-wheel time zoom is driven by our wheel handler
// (which always pins the current/rightmost bar) — native wheel zoom anchors on
// the cursor, which we don't want.
export const DEFAULT_SCALE = {
  mouseWheel:           false,
  pinch:                true,
  axisPressedMouseMove: true,
  axisDoubleClickReset: true,
};

// Scroll handling: the chart's OWN mouse-wheel scroll is disabled so the wheel is
// driven entirely by our handler (2nd/horizontal wheel = vertical price-axis zoom,
// main wheel = time zoom). Click-drag panning and touch dragging stay enabled.
export const DEFAULT_SCROLL = {
  mouseWheel:       false,
  pressedMouseMove: true,
  horzTouchDrag:    true,
  vertTouchDrag:    true,
};

export const PRICE_SCALE_MARGINS = { top: 0.08, bottom: 0.08 };

/** Options every TagDash chart is created with. `withSeconds`: 5 s / 10 s bars. */
export function baseChartOptions(withSeconds: boolean): DeepPartial<ChartOptions> {
  return {
    layout: {
      background:  { color: "#080808" },
      textColor:   "#666",
      fontSize:    10,
      fontFamily:  "ui-monospace, monospace",
      // Attribution moved to Settings ▸ Mise à jour (licence requirement met there).
      attributionLogo: false,
    },
    localization: {
      timeFormatter: (t: Time) => nyDateTime(Number(t), withSeconds),
    },
    grid: {
      // Native grid fully off: vertical lines removed; horizontal lines are drawn
      // by GridPrimitive (snapped to round/half-dollar, never < $0.50).
      vertLines: { visible: false },
      horzLines: { visible: false },
    },
    // Colour + "horizontal line only" come from the theme (Settings → Apparence).
    crosshair: crosshairOptions(getChartTheme()),
    timeScale: {
      borderColor:      "#1e1e1e",
      timeVisible:      true,
      secondsVisible:   withSeconds,
      rightOffset:      7, // bars of empty space past the latest bar
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
      scaleMargins: { ...PRICE_SCALE_MARGINS },
    },
    handleScroll: { ...DEFAULT_SCROLL },
    handleScale:  { ...DEFAULT_SCALE },
  };
}

export const isSecondsTf = (tf: Timeframe) => tf === "5s" || tf === "10s";

/** Candlestick series in the theme's colours (no native last-price line: the
 *  bid/ask lines take its place when quotes exist). */
export function candleSeriesOptions(theme: ChartTheme = getChartTheme()) {
  const { up, down } = theme.candle;
  return {
    upColor:          up,
    downColor:        down,
    borderUpColor:    up,
    borderDownColor:  down,
    wickUpColor:      up,
    wickDownColor:    down,
    priceLineVisible: false,
  };
}

// ─── Volume ─────────────────────────────────────────────────────────────────────

/** Bottom-pinned volume histogram on its own overlay scale. */
export function addVolumeSeries(chart: IChartApi): ISeriesApi<"Histogram"> {
  const s = chart.addHistogramSeries({
    priceFormat:      { type: "volume" },
    priceScaleId:     "", // overlay scale, pinned to the bottom below
    priceLineVisible: false,
    lastValueVisible: false,
  });
  s.priceScale().applyOptions({ scaleMargins: { top: 0.82, bottom: 0 } });
  return s;
}

/** Up/down volume colours: the candle colours at the theme's volume opacity. */
export function volumeColors(theme: ChartTheme = getChartTheme()) {
  return {
    up:   hexToRgba(theme.candle.up, theme.volume.upOpacity),
    down: hexToRgba(theme.candle.down, theme.volume.downOpacity),
  };
}

// ─── Pre / post-market zones ────────────────────────────────────────────────────

/** Per-bar session marks for `SessionBandsPrimitive`: which NY session each bar
 *  belongs to and where a new NY day starts. `times` are UTC seconds, ascending. */
export function sessionMarks(times: number[]): SessionMark[] {
  let prevDay = "";
  return times.map((time, i) => {
    const { day, minutes } = nyDayStamp(time);
    const dayStart = i > 0 && day !== prevDay;
    prevDay = day;
    return { time, session: nySessionOfMinutes(minutes), dayStart };
  });
}

// ─── Strategy-card indicators ───────────────────────────────────────────────────

type IndicatorSeries = ISeriesApi<"Line"> | ISeriesApi<"Histogram">;

/** Draw the pane's line indicators (VWAP / EMA / SMA) and Bollinger fill from
 *  `bars`, reconciling `seriesMap` against what is requested. Previous-day levels
 *  and volume are drawn elsewhere. */
export function applyLineIndicators(
  chart: IChartApi,
  seriesMap: Map<string, IndicatorSeries>,
  bollinger: BollingerPrimitive | null,
  bars: Bar[] | undefined,
  indicators: PaneIndicator[],
  theme: ChartTheme,
) {
  const notLine = (k: PaneIndicator["kind"]) =>
    k === "bollinger_bands" || k === "previous_close" || k === "previous_high" ||
    k === "previous_low" || k === "volume";
  const desired = new Set(indicators.filter((i) => !notLine(i.kind)).map(indicatorId));
  for (const [id, series] of seriesMap) {
    if (!desired.has(id)) {
      chart.removeSeries(series);
      seriesMap.delete(id);
    }
  }
  if (!bars?.length) {
    bollinger?.setData([]);
    return;
  }

  const times  = bars.map((b) => toUTC(b.time));
  const closes = bars.map((b) => b.close);

  // Bollinger → translucent fill band(s) via the primitive.
  const bollingerFill = hexToRgba(theme.indicators.bollinger, theme.indicators.bollingerOpacity);
  const bands: BollingerBand[] = [];
  for (const ind of indicators) {
    if (ind.kind !== "bollinger_bands") continue;
    const { upper, lower } = computeBollinger(closes, ind.period ?? 20, BOLLINGER_K);
    bands.push({ times, upper, lower, fill: bollingerFill });
  }
  bollinger?.setData(bands);

  for (const ind of indicators) {
    if (notLine(ind.kind)) continue;
    const id = indicatorId(ind);
    const color =
      ind.kind === "vwap" ? theme.indicators.vwap
      : ind.kind === "ema" ? theme.indicators.ema
      : ind.kind === "sma" ? theme.indicators.sma
      : "#888";
    let series = seriesMap.get(id) as ISeriesApi<"Line"> | undefined;
    if (!series) {
      series = chart.addLineSeries({
        color,
        lineWidth:              1,
        priceLineVisible:       false,
        lastValueVisible:       false,
        crosshairMarkerVisible: false,
      });
      seriesMap.set(id, series);
    } else {
      series.applyOptions({ color }); // recolour live on a theme edit
    }
    let values: (number | null)[];
    if (ind.kind === "vwap")      values = computeDailyVwap(bars);
    else if (ind.kind === "ema")  values = computeEma(closes, ind.period ?? 9);
    else if (ind.kind === "sma")  values = computeSma(closes, ind.period ?? 20);
    else                           values = bars.map(() => null);
    const data: LineData[] = [];
    values.forEach((v, i) => { if (v != null) data.push({ time: times[i], value: v }); });
    try { series.setData(data); } catch { /* duplicate-time guard */ }
  }
}

/** The pane of a strategy card for `tf`: the same timeframe, else the
 *  interactive (execution) pane, else the first. */
export function cardPane(card: StrategyCard | null | undefined, tf: Timeframe): PaneSpec | null {
  const panes = card?.panes ?? [];
  return panes.find((p) => p.timeframe === tf) ?? panes.find((p) => p.interactive) ?? panes[0] ?? null;
}

// ─── Wheel / trackpad zoom ──────────────────────────────────────────────────────

/** Price-axis zoom: step>0 loosens (zoom out), step<0 tightens (zoom in), by
 *  nudging the price-scale margins symmetrically. */
export function zoomPriceAxis(series: ISeriesApi<SeriesType>, step: number) {
  const scale = series.priceScale();
  const m = scale.options().scaleMargins ?? PRICE_SCALE_MARGINS;
  const clamp = (v: number) => Math.max(0, Math.min(0.45, v));
  scale.applyOptions({ scaleMargins: { top: clamp(m.top + step), bottom: clamp(m.bottom + step) } });
}

/** Time-axis zoom. factor>1 = zoom in. Always keeps the current/rightmost visible
 *  bar fixed and grows/shrinks the range from the left (no cursor anchor, so
 *  mouse wheels and trackpads behave alike). `barCount`: bars on the chart. */
export function zoomTimeAxis(chart: IChartApi, width: number, barCount: number, factor: number) {
  const ts = chart.timeScale();
  const logical = ts.getVisibleLogicalRange();
  if (!logical) return;
  const n = barCount;
  const lastIdx = n - 1;
  if (n === 0 || logical.to >= lastIdx) {
    // Keep the pixel gap past the last bar fixed while zooming, so the current
    // bar's on-screen position doesn't drift. Derived from the live visible range
    // + measured width, NOT from barSpacing/rightOffset options — those lag behind
    // a burst of wheel events (worse on HiDPI screens).
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
}

export interface WheelInput {
  mouseSensitivityX: number;
  mouseSensitivityY: number;
  trackpadSensitivityX: number;
  trackpadSensitivityY: number;
  zoomInvert: boolean;
  scrollSwapAxes: boolean;
}

/** Route a wheel event to the price or time zoom (Settings ▸ Apparence tuning).
 *  Default: horizontal-dominant → price zoom, vertical-dominant → time zoom;
 *  `scrollSwapAxes` flips it. Returns false when the event was ignored. */
export function routeWheel(
  e: WheelEvent,
  input: WheelInput,
  zoomPrice: (step: number) => void,
  zoomTime: (factor: number) => void,
): boolean {
  const absX = Math.abs(e.deltaX);
  const absY = Math.abs(e.deltaY);
  if (absX === 0 && absY === 0) return false;
  const hDominant = absX > absY;
  const onPrice = input.scrollSwapAxes ? !hDominant : hDominant;
  const delta = hDominant ? e.deltaX : e.deltaY;
  if (delta === 0) return false;
  e.preventDefault();
  // Mouse-wheel notches produce large deltas (~100 px); trackpad frames are tiny.
  // Sensitivity is picked by (device × physical wheel axis), so it stays correct
  // under scrollSwapAxes.
  const absD = Math.abs(delta);
  const isMouse = absD >= 50;
  const sens = hDominant
    ? (isMouse ? input.mouseSensitivityX : input.trackpadSensitivityX)
    : (isMouse ? input.mouseSensitivityY : input.trackpadSensitivityY);
  if (onPrice) {
    const step = Math.min(absD * 0.0002 * sens, 0.05);
    zoomPrice(delta < 0 ? -step : step);
    return true;
  }
  const mag = Math.min(absD * 0.001 * sens, 0.5);
  let zoomIn = delta < 0;
  if (input.zoomInvert) zoomIn = !zoomIn;
  zoomTime(zoomIn ? 1 + mag : 1 / (1 + mag));
  return true;
}

/** Wire TagDash's wheel zoom on a chart container (settings read live). Returns
 *  the detach function. */
export function attachWheelZoom(
  container: HTMLElement,
  chart: IChartApi,
  series: ISeriesApi<SeriesType>,
  barCount: () => number,
): () => void {
  const onWheel = (e: WheelEvent) => {
    routeWheel(
      e,
      useChartInput.getState(),
      (step) => zoomPriceAxis(series, step),
      (factor) => zoomTimeAxis(chart, container.clientWidth, barCount(), factor),
    );
  };
  container.addEventListener("wheel", onWheel, { passive: false });
  return () => container.removeEventListener("wheel", onWheel);
}
