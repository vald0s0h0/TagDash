// Custom lightweight-charts series primitive that draws user trend lines with
// full styling (colour + opacity + width + dash) and, for the selected line,
// editable anchor handles (two endpoints + a centre handle). It replaces the
// old one-LineSeries-per-line approach (useDrawingLines) so a single canvas
// surface carries every line of the pane and the selection chrome.
//
// Rendering only. Hit-testing / dragging lives in LightweightChart, which has
// the same chart + series refs and recomputes pixel positions identically.
//
// Same ISeriesPrimitive shape as executionsPrimitive.ts.

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
import type { ChartLine, LineStyleName } from "@/stores/chartStore";

interface BitmapScope {
  context: CanvasRenderingContext2D;
  horizontalPixelRatio: number;
  verticalPixelRatio: number;
}
interface RenderTarget {
  useBitmapCoordinateSpace(cb: (scope: BitmapScope) => void): void;
}

const HANDLE_R = 4; // endpoint handle radius (CSS px)
const MID_R    = 3; // centre handle radius (CSS px)

/** "#rrggbb" + opacity 0..1 → "rgba(...)". Falls back to the raw string. */
export function hexToRgba(hex: string, opacity: number): string {
  const m = /^#?([0-9a-f]{6})$/i.exec(hex.trim());
  if (!m) return hex;
  const n = parseInt(m[1], 16);
  const r = (n >> 16) & 255, g = (n >> 8) & 255, b = n & 255;
  return `rgba(${r},${g},${b},${Math.max(0, Math.min(1, opacity))})`;
}

function dashFor(style: LineStyleName, hr: number): number[] {
  switch (style) {
    case "dashed": return [6 * hr, 4 * hr];
    case "dotted": return [2 * hr, 3 * hr];
    default:       return [];
  }
}

class DrawingsRenderer implements ISeriesPrimitivePaneRenderer {
  constructor(private readonly src: DrawingsPrimitive) {}

  draw(target: RenderTarget): void {
    const chart  = this.src.chart;
    const series = this.src.series;
    if (!chart || !series) return;
    const ts = chart.timeScale();

    target.useBitmapCoordinateSpace((scope) => {
      const ctx = scope.context;
      const hr  = scope.horizontalPixelRatio;
      const vr  = scope.verticalPixelRatio;

      for (const line of this.src.lines) {
        const x1 = ts.timeToCoordinate(line.point1.time as Time);
        const x2 = ts.timeToCoordinate(line.point2.time as Time);
        const y1 = series.priceToCoordinate(line.point1.price);
        const y2 = series.priceToCoordinate(line.point2.price);
        if (x1 == null || x2 == null || y1 == null || y2 == null) continue;

        const ax = x1 * hr, ay = y1 * vr, bx = x2 * hr, by = y2 * vr;
        const selected = line.id === this.src.selectedId;

        // Segment.
        ctx.save();
        ctx.strokeStyle = hexToRgba(line.color, line.opacity);
        ctx.lineWidth = Math.max(1, Math.round(line.width * vr));
        ctx.setLineDash(dashFor(line.lineStyle, hr));
        ctx.beginPath();
        ctx.moveTo(ax, ay);
        ctx.lineTo(bx, by);
        ctx.stroke();
        ctx.restore();

        // Selection chrome: endpoint + centre handles.
        if (selected) {
          ctx.save();
          ctx.setLineDash([]);
          const drawHandle = (x: number, y: number, r: number) => {
            ctx.beginPath();
            ctx.arc(x, y, r * Math.max(hr, vr), 0, Math.PI * 2);
            ctx.fillStyle = "#0a0a0a";
            ctx.fill();
            ctx.lineWidth = Math.max(1, Math.round(1.5 * vr));
            ctx.strokeStyle = "#e5e7eb";
            ctx.stroke();
          };
          drawHandle(ax, ay, HANDLE_R);
          drawHandle(bx, by, HANDLE_R);
          drawHandle((ax + bx) / 2, (ay + by) / 2, MID_R);
          ctx.restore();
        }
      }
    });
  }
}

class DrawingsPaneView implements ISeriesPrimitivePaneView {
  constructor(private readonly src: DrawingsPrimitive) {}
  zOrder() { return "top" as const; }
  renderer(): ISeriesPrimitivePaneRenderer { return new DrawingsRenderer(this.src); }
}

export class DrawingsPrimitive implements ISeriesPrimitive<Time> {
  chart?:  IChartApi;
  series?: ISeriesApi<SeriesType>;
  lines:   ChartLine[] = [];
  selectedId: string | null = null;
  private readonly views: DrawingsPaneView[];
  private requestUpdate?: () => void;

  constructor() {
    this.views = [new DrawingsPaneView(this)];
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

  setData(lines: ChartLine[], selectedId: string | null): void {
    this.lines = lines;
    this.selectedId = selectedId;
    this.requestUpdate?.();
  }
}
