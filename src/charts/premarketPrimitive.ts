// The premarket bars on a DAILY pane: for each day, the 04:00–09:30 ET range as a
// filled column drawn BEHIND the regular-session candle.
//
// Why it is needed at all: both providers' daily bars are regular-session only
// (measured — see `src-tauri/src/premarket/mod.rs`), so a premarket spike that
// faded before the open leaves no trace whatsoever on a daily chart. This draws
// the trace.
//
// THE DESIGN, and why each part of it:
//   • zOrder "bottom" — the whole thing is a backdrop the opaque candle covers.
//     What you see is therefore the part of the premarket range that ESCAPED the
//     day's range: the premarket high nobody took back, the low never revisited.
//     A day whose premarket stayed inside the session range shows nothing, which
//     is correct — there was nothing to see.
//   • 70 % of a regular candle's width — narrower than the candle it sits behind,
//     so it reads as a second, quieter object rather than as a halo around the
//     first. No outline: a stroke would fight the candle border for attention.
//   • open / close as hairline BLACK ticks, left half and right half, the classic
//     OHLC notation. Black because they are read against the bar's own translucent
//     fill, not against the chart background.
// One pane view, all of it behind the candles — including the ticks, which belong
// to the premarket bar and travel with it.
//
// Premarket-only mode changes nothing here: the chart simply stops drawing its
// regular candles (it makes them transparent), and these bars become the content.
//
// Same ISeriesPrimitive shape as sessionBandsPrimitive.ts; colours are read from
// the live theme at draw time.

import type {
  IChartApi,
  ISeriesApi,
  ISeriesPrimitive,
  ISeriesPrimitivePaneView,
  ISeriesPrimitivePaneRenderer,
  SeriesAttachedParameter,
  SeriesType,
  Time,
} from "lightweight-charts";
import { getChartTheme } from "@/stores/chartThemeStore";
import { hexToRgba } from "@/charts/drawingsPrimitive";

// Minimal shape of the bitmap rendering scope (avoids a fancy-canvas import).
interface BitmapScope {
  context: CanvasRenderingContext2D;
  horizontalPixelRatio: number;
  verticalPixelRatio: number;
  bitmapSize: { width: number; height: number };
  mediaSize:  { width: number; height: number };
}
interface RenderTarget {
  useBitmapCoordinateSpace(cb: (scope: BitmapScope) => void): void;
}

/** One premarket bar: an OHLC at a daily slot (UTC seconds, NY midnight). */
export interface GhostBar {
  time:  number;
  open:  number;
  high:  number;
  low:   number;
  close: number;
}

/** Body width of a regular candle, as a fraction of the bar slot. This is what
 *  lightweight-charts' own `optimalCandlestickWidth` converges to once zoomed past
 *  a few pixels of spacing; matching it here is what makes the 70 % below a ratio
 *  of the CANDLE rather than of the slot. */
const CANDLE_BODY_RATIO = 0.8;

/** Premarket bar width, as a fraction of a regular candle's width. */
const PM_WIDTH_RATIO = 0.7;

/** Below this bar width (device px) the open/close ticks stop being notation and
 *  become a smear — the bar itself still draws. */
const TICK_MIN_WIDTH = 4;

class PremarketRenderer implements ISeriesPrimitivePaneRenderer {
  constructor(private readonly src: PremarketPrimitive) {}

  draw(target: RenderTarget): void {
    const { chart, series, bars } = this.src;
    if (!chart || !series || bars.length === 0) return;
    const ts = chart.timeScale();

    target.useBitmapCoordinateSpace((scope) => {
      const ctx = scope.context;
      const hr  = scope.horizontalPixelRatio;
      const vr  = scope.verticalPixelRatio;
      const theme = getChartTheme().premarket;

      const width = Math.max(
        1,
        Math.round(ts.options().barSpacing * CANDLE_BODY_RATIO * PM_WIDTH_RATIO * hr),
      );
      const half = width / 2;
      const lw = Math.max(1, Math.round(vr)); // one device pixel — "très fin"
      const ticks = width >= TICK_MIN_WIDTH;
      const fill = hexToRgba(theme.color, theme.fillOpacity);
      const tick = `rgba(0,0,0,${theme.tickOpacity})`;

      for (const b of bars) {
        const x = ts.timeToCoordinate(b.time as Time);
        if (x == null) continue;
        const centre = (x as number) * hr;
        const left = centre - half;
        // Off-screen: skip before paying for the price conversions.
        if (left > scope.bitmapSize.width || left + width < 0) continue;
        const yh = series.priceToCoordinate(b.high);
        const yl = series.priceToCoordinate(b.low);
        if (yh == null || yl == null) continue;
        const top    = Math.min(yh, yl) * vr;
        const bottom = Math.max(yh, yl) * vr;

        ctx.fillStyle = fill;
        // A range thinner than a pixel is still a range: never draw nothing.
        ctx.fillRect(left, top, width, Math.max(vr, bottom - top));

        if (!ticks) continue;
        const yo = series.priceToCoordinate(b.open);
        const yc = series.priceToCoordinate(b.close);
        ctx.fillStyle = tick;
        if (yo != null) ctx.fillRect(left, (yo as number) * vr, half, lw);
        if (yc != null) ctx.fillRect(centre, (yc as number) * vr, half, lw);
      }
    });
  }
}

class PremarketPaneView implements ISeriesPrimitivePaneView {
  constructor(private readonly src: PremarketPrimitive) {}
  // Backdrop: the candle covers it, so only the out-of-range part shows.
  zOrder() { return "bottom" as const; }
  renderer(): ISeriesPrimitivePaneRenderer { return new PremarketRenderer(this.src); }
}

export class PremarketPrimitive implements ISeriesPrimitive<Time> {
  chart?:  IChartApi;
  series?: ISeriesApi<SeriesType>;
  /** Premarket bars, ascending. Empty on any pane that isn't showing them. */
  bars: GhostBar[] = [];
  private readonly views: ISeriesPrimitivePaneView[];
  private requestUpdate?: () => void;

  constructor() {
    this.views = [new PremarketPaneView(this)];
  }

  attached(param: SeriesAttachedParameter<Time>): void {
    this.chart  = param.chart;
    this.series = param.series;
    this.requestUpdate = param.requestUpdate;
  }
  detached(): void {
    this.chart = undefined;
    this.series = undefined;
    this.requestUpdate = undefined;
  }
  updateAllViews(): void { /* views read live data each draw */ }
  paneViews(): readonly ISeriesPrimitivePaneView[] { return this.views; }

  /** Replace the premarket bars and trigger a redraw. */
  setData(bars: GhostBar[]): void {
    this.bars = bars;
    this.requestUpdate?.();
  }

  /** Force a redraw (e.g. after a theme edit — colours are read at draw time). */
  redraw(): void { this.requestUpdate?.(); }
}
