// Charts of the « Revue » tab, built on TagDash's chart defaults (see
// `charts/chartDefaults.ts`) exactly like the trading panes: same options and NY
// time axis, theme colours for candles and volume, round-level grid, pre/post-market
// zones + day separator, the strategy card's indicators for that timeframe, and the
// wheel/trackpad zoom (time pinned on the latest bar, price on the other axis).

import { createChart, type IChartApi, type ISeriesApi, type UTCTimestamp } from "lightweight-charts";
import type { Bar, PaneIndicator, StrategyCard, Strategy, ReviewDetail, Timeframe } from "@/types";
import {
  addVolumeSeries,
  applyLineIndicators,
  attachWheelZoom,
  baseChartOptions,
  candleSeriesOptions,
  cardPane,
  sessionMarks,
  volumeColors,
} from "@/charts/chartDefaults";
import { crosshairOptions } from "@/charts/chartOptions";
import { BollingerPrimitive } from "@/charts/bollingerPrimitive";
import { ExecutionsPrimitive } from "@/charts/executionsPrimitive";
import { GridPrimitive } from "@/charts/gridPrimitive";
import { SessionBandsPrimitive } from "@/charts/sessionBandsPrimitive";
import { getChartTheme, type ChartTheme } from "@/stores/chartThemeStore";
import type { OhlcBar } from "./model";

/** A bare chart with TagDash's options. A follower (`interactive: false`) neither
 *  scrolls nor zooms by itself — it mirrors the main chart. */
export function makeChart(
  el: HTMLElement,
  withSeconds: boolean,
  opts: { interactive?: boolean; timeVisible?: boolean } = {},
): IChartApi {
  const interactive = opts.interactive ?? true;
  const base = baseChartOptions(withSeconds);
  return createChart(el, {
    ...base,
    autoSize: true,
    timeScale: { ...base.timeScale, visible: opts.timeVisible ?? true },
    ...(interactive ? {} : { handleScroll: false, handleScale: false }),
  });
}

/** Seconds of a review timeframe → the trading timeframe it matches. */
export function tfOf(sec: number): Timeframe {
  return sec <= 5 ? "5s" : sec <= 10 ? "10s" : sec <= 60 ? "1m" : sec <= 120 ? "2m" : sec <= 300 ? "5m" : sec <= 900 ? "15m" : "daily";
}

/** The strategy behind a reviewed item: its alert's strategy, else the one whose
 *  name is the TradeTally setup / review tag (Apex and Dip are Micro Pullback's
 *  entries). */
export function reviewStrategyId(d: ReviewDetail, strategies: Strategy[] | undefined): string | null {
  if (d.session?.strategy_id) return d.session.strategy_id;
  const names = [d.item.setup, d.item.tag].filter(Boolean).map((n) => n!.trim().toLowerCase());
  const hit = strategies?.find((s) => names.includes(s.name.trim().toLowerCase()));
  if (hit) return hit.id;
  return names.some((n) => n.includes("micro") || n === "apex" || n === "dip") ? "micro_pullback" : null;
}

export function paneIndicators(card: StrategyCard | null | undefined, tf: Timeframe): PaneIndicator[] {
  return cardPane(card, tf)?.indicators ?? [];
}

const toBar = (b: OhlcBar): Bar => ({
  time: new Date(b.time * 1000).toISOString(),
  open: b.open,
  high: b.high,
  low: b.low,
  close: b.close,
  volume: b.volume,
  vwap: null,
});

/** Empty bars kept past the current one — the trading panes' `rightOffset`. */
const RIGHT_OFFSET = 7;

/** A candle chart dressed like a trading pane: candles, volume, grid, session
 *  zones, indicators, executions and the wheel zoom.
 *
 *  It FOLLOWS the replay like a live chart: the current bar sits on the right
 *  (with the usual 7 empty bars after it) and the view advances with it, at the
 *  zoom the trader chose. Dragging the chart to look back stops following until
 *  `follow()` is called again (the player's « Suivre », play, a seek). */
export class ReviewCandleChart {
  readonly chart: IChartApi;
  readonly candles: ISeriesApi<"Candlestick">;
  readonly volume: ISeriesApi<"Histogram">;
  readonly exec = new ExecutionsPrimitive();
  private readonly sessions = new SessionBandsPrimitive();
  private readonly grid = new GridPrimitive();
  private readonly bollinger = new BollingerPrimitive();
  private readonly indicatorSeries = new Map<string, ISeriesApi<"Line"> | ISeriesApi<"Histogram">>();
  private readonly detachWheel: () => void;
  private bars: OhlcBar[] = [];
  private len = 0;
  private following = true;
  private spanSet = false;
  private readonly detachDrag: () => void;
  /** Told when following stops or resumes. */
  onFollowChange?: (following: boolean) => void;

  constructor(
    el: HTMLElement,
    withSeconds: boolean,
    private indicators: PaneIndicator[],
    /** Daily bars span a whole day: no session zones. */
    private readonly intraday = true,
    /** Bars shown on the left of the current one at first. */
    private readonly initialSpan = 60,
  ) {
    this.chart = makeChart(el, withSeconds);
    this.candles = this.chart.addCandlestickSeries(candleSeriesOptions());
    // Same stacking as the trading panes: session zones, then the grid, behind
    // everything; then the Bollinger fill and the executions.
    this.candles.attachPrimitive(this.sessions);
    this.candles.attachPrimitive(this.grid);
    this.candles.attachPrimitive(this.exec);
    this.candles.attachPrimitive(this.bollinger);
    this.volume = addVolumeSeries(this.chart);
    this.detachWheel = attachWheelZoom(el, this.chart, this.candles, () => this.len);
    // A drag (not a click) on the chart = the trader looks elsewhere.
    let down: { x: number; y: number } | null = null;
    const onDown = (e: PointerEvent) => { down = { x: e.clientX, y: e.clientY }; };
    const onMove = (e: PointerEvent) => {
      if (!down || !(e.buttons & 1)) return;
      if (Math.abs(e.clientX - down.x) > 4) {
        down = null;
        this.setFollowing(false);
      }
    };
    const onUp = () => { down = null; };
    el.addEventListener("pointerdown", onDown, true);
    el.addEventListener("pointermove", onMove, true);
    window.addEventListener("pointerup", onUp, true);
    this.detachDrag = () => {
      el.removeEventListener("pointerdown", onDown, true);
      el.removeEventListener("pointermove", onMove, true);
      window.removeEventListener("pointerup", onUp, true);
    };
  }

  get isFollowing(): boolean {
    return this.following;
  }

  private setFollowing(on: boolean) {
    if (this.following === on) return;
    this.following = on;
    this.onFollowChange?.(on);
  }

  /** Put the current bar back on the right and keep it there. */
  follow() {
    this.setFollowing(true);
    this.pinRight();
  }

  /** Current bar on the right edge, keeping the number of bars on screen (the
   *  trader's zoom; `initialSpan` the first time). */
  private pinRight() {
    if (!this.len) return;
    const ts = this.chart.timeScale();
    const r = ts.getVisibleLogicalRange();
    const span = this.spanSet && r ? r.to - r.from : this.initialSpan + RIGHT_OFFSET;
    const to = this.len - 1 + RIGHT_OFFSET;
    ts.setVisibleLogicalRange({ from: to - span, to });
    this.spanSet = true;
  }

  get barCount(): number {
    return this.len;
  }

  setIndicators(indicators: PaneIndicator[]) {
    this.indicators = indicators;
    this.refreshIndicators();
  }

  /** Show `bars` (ascending). Only the tail is updated when the head is unchanged
   *  (the replay grows one candle at a time). */
  setBars(bars: OhlcBar[], full = false) {
    const theme = getChartTheme();
    const { up, down } = volumeColors(theme);
    const candle = (b: OhlcBar) => ({ time: b.time as UTCTimestamp, open: b.open, high: b.high, low: b.low, close: b.close });
    const vol = (b: OhlcBar) => ({ time: b.time as UTCTimestamp, value: b.volume, color: b.close >= b.open ? up : down });
    const grew = bars.length !== this.len;
    if (full || bars.length < this.len || this.len === 0 || bars[0]?.time !== this.bars[0]?.time) {
      this.candles.setData(bars.map(candle));
      this.volume.setData(bars.map(vol));
    } else {
      for (let i = Math.max(0, this.len - 1); i < bars.length; i++) {
        this.candles.update(candle(bars[i]));
        this.volume.update(vol(bars[i]));
      }
    }
    this.bars = bars;
    this.len = bars.length;
    if (full || grew) {
      this.sessions.setData(this.intraday ? sessionMarks(bars.map((b) => b.time)) : []);
    }
    this.refreshIndicators();
    if (this.following) this.pinRight();
  }

  private refreshIndicators() {
    applyLineIndicators(this.chart, this.indicatorSeries, this.bollinger, this.bars.map(toBar), this.indicators, getChartTheme());
  }

  /** Re-style after a theme edit (Settings ▸ Apparence). */
  applyTheme(theme: ChartTheme) {
    this.candles.applyOptions(candleSeriesOptions(theme));
    this.chart.applyOptions({ crosshair: crosshairOptions(theme) });
    this.setBars(this.bars, true);
    this.grid.redraw();
    this.sessions.redraw();
    this.exec.redraw();
  }

  remove() {
    this.detachWheel();
    this.detachDrag();
    this.chart.remove();
  }
}
