// Custom lightweight-charts series primitive that draws trade executions:
//  • a small triangle per fill, tip at the fill price (aligned to the bar's x),
//      ▶ pointing right when the fill INCREASED the position,
//      ◀ pointing left  when it DECREASED it;
//      green for a long trade, red for a short trade;
//  • a thin line connecting a trade's fills, coloured green (profit) / red (loss)
//    once the trade is closed, grey while still open.
//
// lightweight-charts markers only offer up/down arrows, so left/right triangles
// with the tip pinned to the price require a primitive (official ISeriesPrimitive
// API: paneViews → renderer → draw via useBitmapCoordinateSpace).

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
import type { TradeExecutions } from "@/types";

// Minimal shape of the bitmap rendering scope (avoids a fancy-canvas import).
interface BitmapScope {
  context: CanvasRenderingContext2D;
  horizontalPixelRatio: number;
  verticalPixelRatio: number;
}
interface RenderTarget {
  useBitmapCoordinateSpace(cb: (scope: BitmapScope) => void): void;
}

const TRI_W = 7; // triangle horizontal size (CSS px)
const TRI_H = 8; // triangle height (CSS px)

const COLOR_BUY  = "#22c55e"; // green
const COLOR_SELL = "#ef4444"; // red
const LINE_PROFIT = "rgba(34,197,94,0.75)";
const LINE_LOSS   = "rgba(239,68,68,0.75)";
const LINE_OPEN   = "rgba(150,150,150,0.45)";
// Original (launch-time) stop loss — a thin, discreet dashed segment spanning the
// trade's duration.
const SL0_COLOR   = "rgba(239,68,68,0.4)";

function toSecNum(iso: string): number {
  return Math.floor(new Date(iso).getTime() / 1000);
}

/** Snap a fill time (seconds) to the chart bar that CONTAINS it — the largest
 *  bar time ≤ fill time. `timeToCoordinate` only resolves exact bar times, so a
 *  raw fill timestamp (which rarely lands on a bar boundary) would return null
 *  and the triangle would never draw. Returns null when the fill is older than
 *  every loaded bar (off the back-filled range → skip until scrolled to). */
function snapToBar(barTimes: number[], fillSec: number): number | null {
  if (barTimes.length === 0) return null;
  if (fillSec < barTimes[0]) return null;
  let lo = 0, hi = barTimes.length - 1, ans = barTimes[0];
  while (lo <= hi) {
    const mid = (lo + hi) >> 1;
    if (barTimes[mid] <= fillSec) { ans = barTimes[mid]; lo = mid + 1; }
    else hi = mid - 1;
  }
  return ans;
}

class ExecutionsRenderer implements ISeriesPrimitivePaneRenderer {
  constructor(private readonly src: ExecutionsPrimitive) {}

  draw(target: RenderTarget): void {
    const chart  = this.src.chart;
    const series = this.src.series;
    if (!chart || !series) return;
    const ts = chart.timeScale();

    target.useBitmapCoordinateSpace((scope) => {
      const ctx = scope.context;
      const hr  = scope.horizontalPixelRatio;
      const vr  = scope.verticalPixelRatio;
      const w   = TRI_W * hr;
      const h   = TRI_H * vr;

      const barTimes = this.src.barTimes;
      for (const trade of this.src.trades) {
        // Project each fill to pixel space; skip those off the visible range.
        const pts: { x: number; y: number; increase: boolean; buy: boolean }[] = [];
        for (const f of trade.fills) {
          const snapped = snapToBar(barTimes, toSecNum(f.time));
          if (snapped == null) continue;
          const xc = ts.timeToCoordinate(snapped as Time);
          const yc = series.priceToCoordinate(f.price);
          if (xc == null || yc == null) continue;
          pts.push({ x: xc * hr, y: yc * vr, increase: f.increase, buy: f.buy });
        }

        // Original-SL segment: a thin dashed line at the launch-time SL spanning
        // the trade's duration (first fill → last fill, or → right edge while the
        // trade is still open). Drawn first so triangles/lines sit on top.
        if (trade.original_sl != null && trade.fills.length > 0 && barTimes.length > 0) {
          const startSec = snapToBar(barTimes, toSecNum(trade.fills[0].time));
          const endSec = trade.closed
            ? snapToBar(barTimes, toSecNum(trade.fills[trade.fills.length - 1].time))
            : barTimes[barTimes.length - 1];
          const y0 = series.priceToCoordinate(trade.original_sl);
          if (startSec != null && endSec != null && y0 != null) {
            const x1 = ts.timeToCoordinate(startSec as Time);
            const x2 = ts.timeToCoordinate(endSec as Time);
            if (x1 != null && x2 != null) {
              ctx.save();
              ctx.strokeStyle = SL0_COLOR;
              ctx.lineWidth = Math.max(1, Math.floor(vr));
              ctx.setLineDash([4 * hr, 4 * hr]);
              ctx.beginPath();
              ctx.moveTo(x1 * hr, y0 * vr);
              ctx.lineTo(x2 * hr, y0 * vr);
              ctx.stroke();
              ctx.restore();
            }
          }
        }

        if (pts.length === 0) continue;

        // Connecting line (drawn first, so triangles sit on top).
        if (pts.length > 1) {
          ctx.beginPath();
          ctx.moveTo(pts[0].x, pts[0].y);
          for (let i = 1; i < pts.length; i++) ctx.lineTo(pts[i].x, pts[i].y);
          ctx.strokeStyle = trade.closed
            ? (trade.pnl >= 0 ? LINE_PROFIT : LINE_LOSS)
            : LINE_OPEN;
          ctx.lineWidth = Math.max(1, Math.floor(vr));
          ctx.stroke();
        }

        // Triangles — tip (apex) at the fill price/bar; body on the opposite
        // side of where it points. Colour is per-fill: green buy / red sell.
        for (const p of pts) {
          ctx.fillStyle = p.buy ? COLOR_BUY : COLOR_SELL;
          ctx.beginPath();
          ctx.moveTo(p.x, p.y); // apex at price
          if (p.increase) {
            // ▶ pointing right → body extends to the left
            ctx.lineTo(p.x - w, p.y - h / 2);
            ctx.lineTo(p.x - w, p.y + h / 2);
          } else {
            // ◀ pointing left → body extends to the right
            ctx.lineTo(p.x + w, p.y - h / 2);
            ctx.lineTo(p.x + w, p.y + h / 2);
          }
          ctx.closePath();
          ctx.fill();
        }
      }
    });
  }
}

class ExecutionsPaneView implements ISeriesPrimitivePaneView {
  constructor(private readonly src: ExecutionsPrimitive) {}
  zOrder() { return "top" as const; }
  renderer(): ISeriesPrimitivePaneRenderer { return new ExecutionsRenderer(this.src); }
}

export class ExecutionsPrimitive implements ISeriesPrimitive<Time> {
  chart?:  IChartApi;
  series?: ISeriesApi<SeriesType>;
  trades:  TradeExecutions[] = [];
  barTimes: number[] = []; // sorted bar times (sec) for snapping fills to bars
  private readonly views: ExecutionsPaneView[];
  private requestUpdate?: () => void;

  constructor() {
    this.views = [new ExecutionsPaneView(this)];
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

  /** Replace the rendered executions + the bar times used to snap fills, then
   *  trigger a redraw. */
  setData(trades: TradeExecutions[], barTimes: number[]): void {
    this.trades = trades;
    this.barTimes = barTimes;
    this.requestUpdate?.();
  }
}
