// Custom horizontal grid drawn beneath the candles. lightweight-charts picks its
// own grid step (often $0.10 / $0.20 / $0.25 when zoomed in); this primitive
// replaces the native horizontal grid so lines only ever land on round- and
// half-dollar levels ($X.00 / $X.50) — never finer than $0.50 — while keeping the
// same adaptive density (a coarser step like $1 / $2 / $5 / $10… as you zoom out).
//
// Same ISeriesPrimitive shape as bollingerPrimitive.ts (zOrder "bottom"). The
// native grid is disabled in LightweightChart's chart options.

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

// Grid line colour/opacity are user-tunable (see chartThemeStore), read at draw.
// Target pixel spacing between lines (drives the chosen step → same density feel
// as the native grid).
const TARGET_SPACING_PX = 50;
// Never finer than a half-dollar.
const MIN_STEP = 0.5;
// Safety cap so a degenerate scale can't try to draw thousands of lines.
const MAX_LINES = 250;

/** Smallest "nice" step (1 / 2 / 5 × 10ⁿ) ≥ `approx`, floored at $0.50. Every
 *  such step is a multiple of 0.5, so all lines fall on round/half-dollar prices. */
function niceStep(approx: number): number {
  const t = Math.max(approx, MIN_STEP);
  const exp = Math.floor(Math.log10(t));
  const base = Math.pow(10, exp);
  const m = t / base; // mantissa in [1, 10)
  const nice = m <= 1 ? 1 : m <= 2 ? 2 : m <= 5 ? 5 : 10;
  return Math.max(nice * base, MIN_STEP);
}

class GridRenderer implements ISeriesPrimitivePaneRenderer {
  constructor(private readonly src: GridPrimitive) {}

  draw(target: RenderTarget): void {
    const series = this.src.series;
    if (!series) return;

    target.useBitmapCoordinateSpace((scope) => {
      const ctx = scope.context;
      const vr  = scope.verticalPixelRatio;
      const h   = scope.mediaSize.height;
      const wPx = scope.bitmapSize.width;
      if (h <= 0) return;

      // Visible price range from the pane's top/bottom pixels.
      const pTop = series.coordinateToPrice(0);
      const pBot = series.coordinateToPrice(h);
      if (pTop == null || pBot == null) return;
      const min = Math.min(pTop, pBot);
      const max = Math.max(pTop, pBot);
      const range = max - min;
      if (!(range > 0)) return;

      const step = niceStep((range / h) * TARGET_SPACING_PX);
      const thickness = Math.max(1, Math.round(vr));
      const grid = getChartTheme().grid;
      ctx.fillStyle = hexToRgba(grid.color, grid.opacity);

      // Index-based stepping keeps prices exact multiples of `step` (no float
      // drift), so lines stay pinned to $X.00 / $X.50.
      const startIdx = Math.ceil(min / step);
      for (let i = startIdx, n = 0; i * step <= max + 1e-9 && n < MAX_LINES; i++, n++) {
        const y = series.priceToCoordinate(i * step);
        if (y == null) continue;
        ctx.fillRect(0, Math.round(y * vr), wPx, thickness);
      }
    });
  }
}

class GridPaneView implements ISeriesPrimitivePaneView {
  constructor(private readonly src: GridPrimitive) {}
  // Draw beneath the candles so it's a backdrop, never over the bars.
  zOrder() { return "bottom" as const; }
  renderer(): ISeriesPrimitivePaneRenderer { return new GridRenderer(this.src); }
}

export class GridPrimitive implements ISeriesPrimitive<Time> {
  chart?:  IChartApi;
  series?: ISeriesApi<SeriesType>;
  private readonly views: GridPaneView[];
  private requestUpdate?: () => void;

  constructor() {
    this.views = [new GridPaneView(this)];
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
  updateAllViews(): void { /* reads the live price scale each draw */ }
  paneViews(): readonly ISeriesPrimitivePaneView[] { return this.views; }

  /** Force a redraw (e.g. after a theme edit — colour is read at draw time). */
  redraw(): void { this.requestUpdate?.(); }
}
