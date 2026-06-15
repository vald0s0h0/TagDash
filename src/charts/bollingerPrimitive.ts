// Custom lightweight-charts series primitive that fills Bollinger bands as a
// single translucent area between the upper (+kσ) and lower (−kσ) envelopes —
// no upper/basis/lower lines, just a faint coloured band (per the design: a 10 %
// violet fill). lightweight-charts has no native "fill between two series", so a
// primitive that draws the closed upper→lower polygon is the clean way (same
// ISeriesPrimitive API the executions overlay uses).

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

// Minimal shape of the bitmap rendering scope (avoids a fancy-canvas import).
interface BitmapScope {
  context: CanvasRenderingContext2D;
  horizontalPixelRatio: number;
  verticalPixelRatio: number;
}
interface RenderTarget {
  useBitmapCoordinateSpace(cb: (scope: BitmapScope) => void): void;
}

/** One Bollinger band to fill: parallel arrays (bar time in seconds + upper /
 *  lower price, null during the warm-up) and the fill colour. */
export interface BollingerBand {
  times: number[];
  upper: (number | null)[];
  lower: (number | null)[];
  fill:  string;
}

class BollingerRenderer implements ISeriesPrimitivePaneRenderer {
  constructor(private readonly src: BollingerPrimitive) {}

  draw(target: RenderTarget): void {
    const chart  = this.src.chart;
    const series = this.src.series;
    if (!chart || !series) return;
    const ts = chart.timeScale();

    target.useBitmapCoordinateSpace((scope) => {
      const ctx = scope.context;
      const hr  = scope.horizontalPixelRatio;
      const vr  = scope.verticalPixelRatio;

      for (const band of this.src.bands) {
        // Project every bar where both envelopes exist into pixel space; a
        // warm-up null (or an unresolved off-data coordinate) just breaks the
        // run — Bollinger has only leading nulls, so this is a single polygon.
        const top: { x: number; y: number }[] = [];
        const bot: { x: number; y: number }[] = [];
        for (let i = 0; i < band.times.length; i++) {
          const u = band.upper[i];
          const l = band.lower[i];
          if (u == null || l == null) continue;
          const x  = ts.timeToCoordinate(band.times[i] as Time);
          const yu = series.priceToCoordinate(u);
          const yl = series.priceToCoordinate(l);
          if (x == null || yu == null || yl == null) continue;
          top.push({ x: x * hr, y: yu * vr });
          bot.push({ x: x * hr, y: yl * vr });
        }
        if (top.length < 2) continue;

        ctx.beginPath();
        ctx.moveTo(top[0].x, top[0].y);
        for (let i = 1; i < top.length; i++) ctx.lineTo(top[i].x, top[i].y);
        for (let i = bot.length - 1; i >= 0; i--) ctx.lineTo(bot[i].x, bot[i].y);
        ctx.closePath();
        ctx.fillStyle = band.fill;
        ctx.fill();
      }
    });
  }
}

class BollingerPaneView implements ISeriesPrimitivePaneView {
  constructor(private readonly src: BollingerPrimitive) {}
  // Draw beneath the candles so the band is a backdrop, never over the bars.
  zOrder() { return "bottom" as const; }
  renderer(): ISeriesPrimitivePaneRenderer { return new BollingerRenderer(this.src); }
}

export class BollingerPrimitive implements ISeriesPrimitive<Time> {
  chart?:  IChartApi;
  series?: ISeriesApi<SeriesType>;
  bands:   BollingerBand[] = [];
  private readonly views: BollingerPaneView[];
  private requestUpdate?: () => void;

  constructor() {
    this.views = [new BollingerPaneView(this)];
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

  /** Replace the rendered bands and trigger a redraw. */
  setData(bands: BollingerBand[]): void {
    this.bands = bands;
    this.requestUpdate?.();
  }
}
