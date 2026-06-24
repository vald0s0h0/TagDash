// Custom lightweight-charts series primitive that drops a small news "pastille" —
// a little filled dot at the BOTTOM of the pane (over the volume bars) — on each
// bar that had a news headline. Works the same on intraday and daily panes: the
// caller snaps every news timestamp to its bar and passes the unique bar times.
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

class NewsRenderer implements ISeriesPrimitivePaneRenderer {
  constructor(private readonly src: NewsPrimitive) {}

  draw(target: RenderTarget): void {
    const chart = this.src.chart;
    if (!chart || this.src.times.length === 0) return;
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
      for (const t of this.src.times) {
        const x = ts.timeToCoordinate(t as Time);
        if (x == null) continue; // off the visible range
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
  /** Unique bar times (sec) that carry ≥1 news headline. */
  times:   number[] = [];
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

  /** Replace the news bar times and trigger a redraw. */
  setData(times: number[]): void {
    this.times = times;
    this.requestUpdate?.();
  }

  /** Force a redraw (e.g. after a theme edit — colour is read at draw time). */
  redraw(): void { this.requestUpdate?.(); }
}
