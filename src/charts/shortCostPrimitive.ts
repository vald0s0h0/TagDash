// Full-width short break-even band. It marks prices that are below the entry but
// still loss-making after known locate/borrow costs. Rendered beneath candles with
// a deliberately low-opacity red fill so price action remains readable.

import type {
  IChartApi,
  ISeriesApi,
  ISeriesPrimitive,
  ISeriesPrimitivePaneRenderer,
  ISeriesPrimitivePaneView,
  SeriesAttachedParameter,
  SeriesType,
  Time,
} from "lightweight-charts";

interface BitmapScope {
  context: CanvasRenderingContext2D;
  horizontalPixelRatio: number;
  verticalPixelRatio: number;
  bitmapSize: { width: number; height: number };
}
interface RenderTarget {
  useBitmapCoordinateSpace(cb: (scope: BitmapScope) => void): void;
}

export interface ShortCostBand {
  entryPrice: number;
  breakEvenPrice: number;
  label: string;
  complete: boolean;
}

class ShortCostRenderer implements ISeriesPrimitivePaneRenderer {
  constructor(private readonly src: ShortCostPrimitive) {}

  draw(target: RenderTarget): void {
    const series = this.src.series;
    const band = this.src.band;
    if (!series || !band || band.entryPrice <= band.breakEvenPrice) return;
    target.useBitmapCoordinateSpace((scope) => {
      const yEntry = series.priceToCoordinate(band.entryPrice);
      const yBreakEven = series.priceToCoordinate(band.breakEvenPrice);
      if (yEntry == null || yBreakEven == null) return;
      const ctx = scope.context;
      const hr = scope.horizontalPixelRatio;
      const vr = scope.verticalPixelRatio;
      const top = Math.min(yEntry, yBreakEven) * vr;
      const bottom = Math.max(yEntry, yBreakEven) * vr;

      ctx.save();
      ctx.fillStyle = band.complete ? "rgba(239,68,68,0.14)" : "rgba(239,68,68,0.10)";
      ctx.fillRect(0, top, scope.bitmapSize.width, Math.max(vr, bottom - top));
      ctx.strokeStyle = band.complete ? "rgba(248,113,113,0.75)" : "rgba(248,113,113,0.55)";
      ctx.lineWidth = Math.max(1, vr);
      ctx.setLineDash([5 * hr, 4 * hr]);
      ctx.beginPath();
      ctx.moveTo(0, bottom);
      ctx.lineTo(scope.bitmapSize.width, bottom);
      ctx.stroke();

      const height = bottom - top;
      if (height >= 13 * vr) {
        ctx.setLineDash([]);
        ctx.font = `${9 * vr}px ui-monospace, monospace`;
        ctx.textBaseline = "top";
        ctx.fillStyle = "rgba(254,202,202,0.82)";
        ctx.fillText(band.label, 6 * hr, top + 2 * vr);
      }
      ctx.restore();
    });
  }
}

class ShortCostPaneView implements ISeriesPrimitivePaneView {
  constructor(private readonly src: ShortCostPrimitive) {}
  zOrder() { return "bottom" as const; }
  renderer(): ISeriesPrimitivePaneRenderer { return new ShortCostRenderer(this.src); }
}

export class ShortCostPrimitive implements ISeriesPrimitive<Time> {
  chart?: IChartApi;
  series?: ISeriesApi<SeriesType>;
  band: ShortCostBand | null = null;
  private readonly views = [new ShortCostPaneView(this)];
  private requestUpdate?: () => void;

  attached(param: SeriesAttachedParameter<Time>): void {
    this.chart = param.chart;
    this.series = param.series;
    this.requestUpdate = param.requestUpdate;
  }
  detached(): void {
    this.chart = undefined;
    this.series = undefined;
    this.requestUpdate = undefined;
  }
  updateAllViews(): void { /* view reads the current band */ }
  paneViews(): readonly ISeriesPrimitivePaneView[] { return this.views; }

  setData(band: ShortCostBand | null): void {
    this.band = band;
    this.requestUpdate?.();
  }
}
