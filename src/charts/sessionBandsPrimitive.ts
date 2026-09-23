// Extended-hours background + day-change separator, drawn as a backdrop under the
// candles.
//
// This replaces the old histogram-based shading (one coloured bar per extended-hours
// candle), which rendered as vertical STRIPES: a histogram column is only as wide as
// the bar itself, so the gaps between bars stayed unpainted. Here each contiguous run
// of pre-market (or after-hours) bars is filled as ONE solid rectangle, from half a
// bar-spacing left of the run's first bar to half a bar-spacing right of its last —
// an unbroken zone, whatever the zoom.
//
// The same pass draws the day separator: a hairline at the boundary between the last
// bar of a NY calendar day and the first bar of the next (i.e. after-hours → next
// pre-market). Intraday only — the hook feeds no data on a daily pane.
//
// Same ISeriesPrimitive shape as gridPrimitive.ts (zOrder "bottom"); colours are read
// from the live theme at draw time.

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
import type { NySession } from "@/lib/nyTime";

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

/** One entry per chart bar, pre-computed by the hook (the NY-session and day-key
 *  lookups go through Intl — far too costly to redo on every frame). */
export interface SessionMark {
  /** Bar time, UTC seconds (same value the candle series was fed). */
  time:    number;
  session: NySession;
  /** True when this bar opens a new NY calendar day → separator drawn at its left edge. */
  dayStart: boolean;
}

/** Bars scanned on each side of the visible logical range — covers the live
 *  forming candle (which can sit past the fed `bars`) and any rounding. */
const SCAN_PAD = 3;

class SessionBandsRenderer implements ISeriesPrimitivePaneRenderer {
  constructor(private readonly src: SessionBandsPrimitive) {}

  draw(target: RenderTarget): void {
    const chart = this.src.chart;
    const marks = this.src.marks;
    if (!chart || marks.length === 0) return;
    const ts = chart.timeScale();

    target.useBitmapCoordinateSpace((scope) => {
      const ctx = scope.context;
      const hr  = scope.horizontalPixelRatio;
      const vr  = scope.verticalPixelRatio;
      const h   = scope.mediaSize.height;
      if (h <= 0) return;

      // Only walk the bars that can land on screen: `getVisibleLogicalRange()`
      // indexes the series data, which is fed straight from the same bar array.
      const lr = ts.getVisibleLogicalRange();
      const last = marks.length - 1;
      const from = lr ? Math.max(0, Math.floor(lr.from) - SCAN_PAD) : 0;
      const to   = lr ? Math.min(last, Math.ceil(lr.to) + SCAN_PAD) : last;
      if (from > to) return;

      // Half a bar slot: the band edge sits between two bars, not on a bar centre,
      // so consecutive runs meet exactly and leave no seam.
      const half = ts.options().barSpacing / 2;
      const theme = getChartTheme();
      const fill: Record<string, string> = {
        pre:  hexToRgba(theme.session.preColor,  theme.session.preOpacity),
        post: hexToRgba(theme.session.postColor, theme.session.postOpacity),
      };
      const hPx = h * vr;

      // ── Extended-hours zones: one rect per contiguous same-session run ───────
      let runStart = -1;
      const flush = (endIdx: number) => {
        if (runStart < 0) return;
        const x0 = ts.timeToCoordinate(marks[runStart].time as Time);
        const x1 = ts.timeToCoordinate(marks[endIdx].time as Time);
        runStart = -1;
        if (x0 == null || x1 == null) return;
        const left  = ((x0 as number) - half) * hr;
        const right = ((x1 as number) + half) * hr;
        if (right <= left) return;
        ctx.fillRect(left, 0, right - left, hPx);
      };
      for (let i = from; i <= to; i++) {
        const s = marks[i].session;
        const open = runStart >= 0 ? marks[runStart].session : null;
        // A run also ends at a day boundary: pre-market of the next day must not
        // merge with the after-hours it follows (different colours anyway).
        if (open !== null && (s !== open || marks[i].dayStart)) flush(i - 1);
        if (s === "regular") continue;
        if (runStart < 0) {
          runStart = i;
          ctx.fillStyle = fill[s];
        }
      }
      flush(to);

      // ── Day-change separator (hairline, over the zones it separates) ─────────
      const sep = theme.daySeparator;
      if (!sep.enabled) return;
      ctx.fillStyle = hexToRgba(sep.color, sep.opacity);
      const w = Math.max(1, Math.round(hr * 0.5)); // 1 device pixel — "très fine"
      for (let i = Math.max(from, 1); i <= to; i++) {
        if (!marks[i].dayStart) continue;
        const x = ts.timeToCoordinate(marks[i].time as Time);
        if (x == null) continue;
        ctx.fillRect(Math.round(((x as number) - half) * hr), 0, w, hPx);
      }
    });
  }
}

class SessionBandsPaneView implements ISeriesPrimitivePaneView {
  constructor(private readonly src: SessionBandsPrimitive) {}
  // Backdrop: under the candles, the volume and the grid.
  zOrder() { return "bottom" as const; }
  renderer(): ISeriesPrimitivePaneRenderer { return new SessionBandsRenderer(this.src); }
}

export class SessionBandsPrimitive implements ISeriesPrimitive<Time> {
  chart?:  IChartApi;
  series?: ISeriesApi<SeriesType>;
  /** One mark per bar, in chart order. Empty on a daily pane. */
  marks:   SessionMark[] = [];
  private readonly views: SessionBandsPaneView[];
  private requestUpdate?: () => void;

  constructor() {
    this.views = [new SessionBandsPaneView(this)];
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

  /** Replace the per-bar session marks and trigger a redraw. */
  setData(marks: SessionMark[]): void {
    this.marks = marks;
    this.requestUpdate?.();
  }

  /** Force a redraw (e.g. after a theme edit — colours are read at draw time). */
  redraw(): void { this.requestUpdate?.(); }
}
