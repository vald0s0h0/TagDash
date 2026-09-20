// The Dip band: the full-width green zone the trader lays on a price he expects to
// be struck. Drawn BEHIND the candles, at low opacity, so the price action that
// walks into it stays completely readable — the band is a target, not a mask.
//
// Three looks, and each one says exactly one thing:
//   • armed      — a calm fill with two solid edges: the level is watched;
//   • attacking  — the price is inside it right now, so the fill and the edges come
//                  up and the label carries the measured speed. This is the second
//                  in which an order may leave;
//   • en veille  — everything dimmed: the band is drawn and watched, but a bracket
//                  distance is under the minimum and nothing can fire.
//
// Green, where Apex's armed lines are violet: two engines of the same strategy,
// never confusable at a glance.

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

export interface DipZone {
  low: number;
  high: number;
  /** Long = the band is a support (under the price), Short = a resistance. */
  side: "long" | "short";
  /** False while a bracket distance is under the minimum: drawn, but inert. */
  tradeable: boolean;
  /** The price is inside the band right now. */
  attacking: boolean;
  /** One line above the band, e.g. "DIP support · 6 ¢ · LONG". */
  label: string;
}

/** The one green Dip is drawn in, at the three opacities the states need. */
const GREEN = "74,222,128"; // emerald-400

class DipZoneRenderer implements ISeriesPrimitivePaneRenderer {
  constructor(private readonly src: DipZonePrimitive) {}

  draw(target: RenderTarget): void {
    const series = this.src.series;
    const zone = this.src.zone;
    if (!series || !zone || !(zone.high > zone.low)) return;
    target.useBitmapCoordinateSpace((scope) => {
      const yHigh = series.priceToCoordinate(zone.high);
      const yLow = series.priceToCoordinate(zone.low);
      if (yHigh == null || yLow == null) return;
      const ctx = scope.context;
      const hr = scope.horizontalPixelRatio;
      const vr = scope.verticalPixelRatio;
      const top = Math.min(yHigh, yLow) * vr;
      const bottom = Math.max(yHigh, yLow) * vr;
      // A band thinner than a pixel is still a level: never draw nothing.
      const height = Math.max(vr, bottom - top);

      const dim = !zone.tradeable;
      const fill = dim ? 0.07 : zone.attacking ? 0.26 : 0.14;
      const edge = dim ? 0.3 : zone.attacking ? 0.95 : 0.6;

      ctx.save();
      ctx.fillStyle = `rgba(${GREEN},${fill})`;
      ctx.fillRect(0, top, scope.bitmapSize.width, height);

      ctx.strokeStyle = `rgba(${GREEN},${edge})`;
      ctx.lineWidth = Math.max(1, vr);
      ctx.beginPath();
      ctx.moveTo(0, top);
      ctx.lineTo(scope.bitmapSize.width, top);
      ctx.moveTo(0, bottom);
      ctx.lineTo(scope.bitmapSize.width, bottom);
      ctx.stroke();

      if (zone.label) {
        ctx.font = `${9 * vr}px ui-monospace, monospace`;
        ctx.fillStyle = `rgba(${GREEN},${dim ? 0.45 : 0.9})`;
        // Above the band for a support, below it for a resistance: the caption
        // always sits on the side the price is coming from.
        if (zone.side === "long") {
          ctx.textBaseline = "bottom";
          ctx.fillText(zone.label, 6 * hr, top - 2 * vr);
        } else {
          ctx.textBaseline = "top";
          ctx.fillText(zone.label, 6 * hr, bottom + 2 * vr);
        }
      }
      ctx.restore();
    });
  }
}

class DipZonePaneView implements ISeriesPrimitivePaneView {
  constructor(private readonly src: DipZonePrimitive) {}
  zOrder() { return "bottom" as const; }
  renderer(): ISeriesPrimitivePaneRenderer { return new DipZoneRenderer(this.src); }
}

export class DipZonePrimitive implements ISeriesPrimitive<Time> {
  chart?: IChartApi;
  series?: ISeriesApi<SeriesType>;
  zone: DipZone | null = null;
  private readonly views = [new DipZonePaneView(this)];
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
  updateAllViews(): void { /* view reads the current zone */ }
  paneViews(): readonly ISeriesPrimitivePaneView[] { return this.views; }

  setData(zone: DipZone | null): void {
    this.zone = zone;
    this.requestUpdate?.();
  }
}
