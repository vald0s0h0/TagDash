// Custom lightweight-charts series primitive that drops a small news "pastille" —
// a little filled dot at the BOTTOM of the pane (over the volume bars) — for each
// bar that had ≥1 news headline (a single dot per bar, never stacked). On daily
// panes the dot sits on the day's bar; on intraday panes it is placed at the
// PRECISE publish moment INSIDE the bar by interpolating between the bar and the
// next one (lightweight-charts can't map a non-bar timestamp to an x directly).
//
// A primitive (not setMarkers) because we want the dot pinned to the pane bottom
// regardless of price, drawn on top of the volume — which native markers can't do.
// Same ISeriesPrimitive shape as executionsPrimitive.ts / gridPrimitive.ts.

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

// Minimal shape of the bitmap rendering scope (avoids a fancy-canvas import).
interface BitmapScope {
  context: CanvasRenderingContext2D;
  horizontalPixelRatio: number;
  verticalPixelRatio: number;
  mediaSize: { width: number; height: number };
}
interface RenderTarget {
  useBitmapCoordinateSpace(cb: (scope: BitmapScope) => void): void;
}

const DOT_R      = 2.6; // dot radius (CSS px)
const PAD_BOTTOM = 4;   // dot centre offset above the pane's bottom edge (CSS px)

/** One news pastille: anchored on bar `t0`, optionally nudged `frac` of the way
 *  toward the next bar `t1` to land on the precise intraday publish moment. */
export interface NewsMark {
  /** Bar time (sec) the news belongs to (daily snaps exactly here). */
  t0:   number;
  /** Next bar time (sec) for intraday interpolation; null at the series tail. */
  t1:   number | null;
  /** Fraction from t0 toward t1 (0 = on the bar; daily is always 0). */
  frac: number;
}

class NewsRenderer implements ISeriesPrimitivePaneRenderer {
  constructor(private readonly src: NewsPrimitive) {}

  draw(target: RenderTarget): void {
    const chart = this.src.chart;
    if (!chart || this.src.marks.length === 0) return;
    const ts = chart.timeScale();

    target.useBitmapCoordinateSpace((scope) => {
      const ctx = scope.context;
      const hr  = scope.horizontalPixelRatio;
      const vr  = scope.verticalPixelRatio;
      const h   = scope.mediaSize.height;
      if (h <= 0) return;

      const y = (h - PAD_BOTTOM) * vr;
      const r = DOT_R * hr;
      ctx.fillStyle = getChartTheme().markers.news;
      for (const m of this.src.marks) {
        const x0 = ts.timeToCoordinate(m.t0 as Time);
        if (x0 == null) continue; // off the visible range
        let x = x0 as number;
        // Interpolate toward the next bar so the dot sits at the exact publish
        // time (both t0 and t1 are real bars, so each maps to a coordinate).
        if (m.t1 != null && m.frac > 0) {
          const x1 = ts.timeToCoordinate(m.t1 as Time);
          if (x1 != null) x = (x0 as number) + m.frac * ((x1 as number) - (x0 as number));
        }
        ctx.beginPath();
        ctx.arc(x * hr, y, r, 0, Math.PI * 2);
        ctx.fill();
      }
    });
  }
}

class NewsPaneView implements ISeriesPrimitivePaneView {
  constructor(private readonly src: NewsPrimitive) {}
  // Over the volume bars.
  zOrder() { return "top" as const; }
  renderer(): ISeriesPrimitivePaneRenderer { return new NewsRenderer(this.src); }
}

export class NewsPrimitive implements ISeriesPrimitive<Time> {
  chart?:  IChartApi;
  series?: ISeriesApi<SeriesType>;
  /** One pastille per bar that carries ≥1 news headline. */
  marks:   NewsMark[] = [];
  private readonly views: NewsPaneView[];
  private requestUpdate?: () => void;

  constructor() {
    this.views = [new NewsPaneView(this)];
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

  /** Replace the news pastilles and trigger a redraw. */
  setData(marks: NewsMark[]): void {
    this.marks = marks;
    this.requestUpdate?.();
  }

  /** Force a redraw (e.g. after a theme edit — colour is read at draw time). */
  redraw(): void { this.requestUpdate?.(); }
}
