// Spread pane of the review replay — EVERY quote, on the main chart's time axis.
//
// A lightweight-charts series would put each quote on its own bar index and break
// the alignment with the candles, so the spread is drawn on a canvas instead, from
// the main chart's own time scale: a quote at instant t sits in its candle at the
// fraction of the candle elapsed (t − open) / timeframe, so it scrolls and zooms
// with the candles above it and lines up with them exactly. A quote in a bucket
// with no candle (no print) sits at the end of the previous candle.
//
// The line is a step (the spread holds until the next quote); many quotes on the
// same pixel column draw their full min–max range there, so no spike is lost.

import type { IChartApi, Logical } from "lightweight-charts";
import type { ReviewSessionData } from "@/types";
import { lastIndexAtOrBefore } from "./model";

const BG = "#080808";
const GRID = "#141414";
const AXIS_BORDER = "#1e1e1e";
const TEXT = "#666";
const LINE = "#f59e0b";
const FONT = "ui-monospace, monospace";

interface State {
  main: IChartApi;
  /** Candle open times (unix s), as on the main chart. */
  barTimes: number[];
  tfSec: number;
  data: ReviewSessionData;
  /** Playhead (ms): nothing after it is drawn. */
  at: number;
}

function niceStep(range: number, ticks: number): number {
  const raw = range / Math.max(1, ticks);
  const mag = 10 ** Math.floor(Math.log10(raw));
  const n = raw / mag;
  return (n <= 1 ? 1 : n <= 2 ? 2 : n <= 5 ? 5 : 10) * mag;
}

export class SpreadPane {
  private canvas: HTMLCanvasElement;
  private ro: ResizeObserver;
  private state: State | null = null;
  private raf = 0;

  constructor(private host: HTMLElement) {
    this.canvas = document.createElement("canvas");
    this.canvas.style.cssText = "position:absolute;inset:0;width:100%;height:100%";
    host.appendChild(this.canvas);
    this.ro = new ResizeObserver(() => this.schedule());
    this.ro.observe(host);
  }

  update(state: State): void {
    this.state = state;
    this.schedule();
  }

  /** Redraw on the next frame (coalesces scroll / zoom / playhead updates). */
  schedule(): void {
    if (this.raf) return;
    this.raf = requestAnimationFrame(() => {
      this.raf = 0;
      this.draw();
    });
  }

  destroy(): void {
    cancelAnimationFrame(this.raf);
    this.ro.disconnect();
    this.canvas.remove();
  }

  private draw(): void {
    const dpr = window.devicePixelRatio || 1;
    const cssW = this.host.clientWidth;
    const cssH = this.host.clientHeight;
    const W = Math.max(1, Math.round(cssW * dpr));
    const H = Math.max(1, Math.round(cssH * dpr));
    if (this.canvas.width !== W) this.canvas.width = W;
    if (this.canvas.height !== H) this.canvas.height = H;
    const ctx = this.canvas.getContext("2d");
    if (!ctx) return;
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.fillStyle = BG;
    ctx.fillRect(0, 0, W, H);

    const st = this.state;
    const scaleW = st ? st.main.priceScale("right").width() : 72;
    const plotW = Math.max(1, (cssW - scaleW) * dpr);
    const top = 14 * dpr;
    const bottom = H - 4 * dpr;

    ctx.font = `${10 * dpr}px ${FONT}`;
    ctx.fillStyle = TEXT;
    ctx.textBaseline = "top";
    ctx.fillText("spread ¢", 6 * dpr, 2 * dpr);
    ctx.fillStyle = AXIS_BORDER;
    ctx.fillRect(plotW, 0, Math.max(1, dpr), H);
    if (!st || !st.barTimes.length) return;

    const { main, barTimes, tfSec, data, at } = st;
    const ts = main.timeScale();
    const range = ts.getVisibleLogicalRange();
    if (!range) return;
    const n = barTimes.length;
    const i0 = Math.max(0, Math.floor(range.from));
    const i1 = Math.min(n - 1, Math.ceil(range.to));
    if (i1 < i0) return;
    const c0 = ts.logicalToCoordinate(i0 as Logical);
    const c1 = ts.logicalToCoordinate((i0 + 1) as Logical);
    if (c0 == null || c1 == null) return;
    const spacing = c1 - c0;
    // Device-pixel x of an instant, inside its candle.
    const xOf = (ms: number): number => {
      const sec = ms / 1000;
      const i = Math.max(0, lastIndexAtOrBefore(barTimes, sec));
      const frac = Math.min(1, Math.max(0, (sec - barTimes[i]) / tfSec));
      return (c0 + (i - i0) * spacing + (frac - 0.5) * spacing) * dpr;
    };

    const tA = barTimes[i0] * 1000;
    const tB = Math.min(at, (barTimes[i1] + tfSec) * 1000);
    if (tB < tA) return;
    const qa = Math.max(0, lastIndexAtOrBefore(data.quotes_ts, tA));
    const qb = lastIndexAtOrBefore(data.quotes_ts, tB);
    if (qb < 0 || qb < qa) return;

    const valueAt = (q: number): number | null => {
      const b = data.quotes_bid[q];
      const a = data.quotes_ask[q];
      return b > 0 && a > 0 ? (a - b) * 100 : null;
    };
    let lo = 0;
    let hi = -Infinity;
    for (let q = qa; q <= qb; q++) {
      const v = valueAt(q);
      if (v == null) continue;
      if (v < lo) lo = v;
      if (v > hi) hi = v;
    }
    if (!Number.isFinite(hi)) return;
    if (hi - lo < 1) hi = lo + 1;
    hi += (hi - lo) * 0.08;
    const y = (v: number) => bottom - ((v - lo) / (hi - lo)) * (bottom - top);

    // Grid + axis labels.
    const step = niceStep(hi - lo, 3);
    ctx.textBaseline = "middle";
    for (let v = Math.ceil(lo / step) * step; v <= hi; v += step) {
      const yy = Math.round(y(v));
      ctx.fillStyle = GRID;
      ctx.fillRect(0, yy, plotW, Math.max(1, dpr));
      ctx.fillStyle = TEXT;
      ctx.fillText(v.toFixed(step < 1 ? 1 : 0), plotW + 6 * dpr, yy);
    }

    // Step line, one min–max run per pixel column.
    ctx.save();
    ctx.beginPath();
    ctx.rect(0, 0, plotW, H);
    ctx.clip();
    ctx.strokeStyle = LINE;
    ctx.lineWidth = Math.max(1, dpr);
    ctx.beginPath();
    let started = false;
    let prev: number | null = null;
    let col = -Infinity;
    let cMin = 0;
    let cMax = 0;
    let cLast = 0;
    const flush = () => {
      if (!Number.isFinite(col)) return;
      ctx.lineTo(col, y(cMin));
      ctx.lineTo(col, y(cMax));
      ctx.lineTo(col, y(cLast));
    };
    for (let q = qa; q <= qb; q++) {
      const v = valueAt(q);
      if (v == null) continue;
      const x = Math.round(xOf(Math.max(tA, data.quotes_ts[q])));
      if (!started) {
        ctx.moveTo(x, y(v));
        started = true;
      }
      if (x !== col) {
        flush();
        if (prev != null) ctx.lineTo(x, y(prev));
        col = x;
        cMin = cMax = v;
        if (prev != null) {
          cMin = Math.min(cMin, prev);
          cMax = Math.max(cMax, prev);
        }
      } else {
        cMin = Math.min(cMin, v);
        cMax = Math.max(cMax, v);
      }
      cLast = v;
      prev = v;
    }
    flush();
    if (prev != null) ctx.lineTo(xOf(tB), y(prev));
    ctx.stroke();
    ctx.restore();

    // Current value on the axis, like a series' last-value label.
    if (prev != null) {
      const label = prev.toFixed(1);
      const yy = Math.min(bottom, Math.max(top, y(prev)));
      const h = 14 * dpr;
      ctx.fillStyle = LINE;
      ctx.fillRect(plotW, yy - h / 2, W - plotW, h);
      ctx.fillStyle = "#000";
      ctx.fillText(label, plotW + 6 * dpr, yy);
    }
  }
}
