// Pure indicator math + styling constants for chart panes. No React, no chart
// instance — just functions over bar/close arrays, so they're trivially testable
// and reusable across panes (and future option charts).

import type { Bar, PaneIndicator } from "@/types";
import { nyDateKey } from "@/lib/nyTime";

export const INDICATOR_COLORS = {
  vwap: "#3b82f6", // blue
  ema:  "#60a5fa", // light blue
  sma:  "#9ca3af", // grey
} as const;

/** Bollinger period multiplier (classic 2σ). */
export const BOLLINGER_K = 2;
/** Bollinger is drawn as a single translucent violet band (no upper/basis/lower
 *  lines) — a faint 10 % fill between ±2σ. */
export const BOLLINGER_FILL = "rgba(167,139,250,0.10)"; // violet, 10 % opacity

/** Stable id for an indicator so series can be created/updated/removed in place. */
export function indicatorId(ind: PaneIndicator): string {
  return ind.period != null ? `${ind.kind}-${ind.period}` : ind.kind;
}

/** Daily (session-anchored) VWAP: the classic running VWAP that resets at each
 *  NY trading-day boundary. For each bar, value = Σ(typical·volume) / Σ(volume)
 *  accumulated from the start of that day, where typical = (high+low+close)/3.
 *  Bars with no volume contribute nothing and carry the prior cumulative value.
 *  Returns one value per bar (null only before any volume has traded that day).
 *
 *  This replaces the previous per-bar `bar.vwap` (Alpaca's single-bar VWAP),
 *  which is NOT a session VWAP and drifted with each candle. */
export function computeDailyVwap(bars: Bar[]): (number | null)[] {
  const out: (number | null)[] = [];
  let cumPV = 0;
  let cumV  = 0;
  let day   = "";
  for (const b of bars) {
    const d = nyDateKey(b.time);
    if (d !== day) { cumPV = 0; cumV = 0; day = d; }
    const typical = (b.high + b.low + b.close) / 3;
    const vol     = b.volume ?? 0;
    cumPV += typical * vol;
    cumV  += vol;
    out.push(cumV > 0 ? cumPV / cumV : null);
  }
  return out;
}

export function computeEma(closes: number[], period: number): (number | null)[] {
  const out: (number | null)[] = [];
  const k = 2 / (period + 1);
  let prev: number | null = null;
  closes.forEach((c, i) => {
    if (i < period - 1) { out.push(null); return; }
    if (prev == null) {
      // seed with the SMA of the first `period` closes
      const seed = closes.slice(i - period + 1, i + 1).reduce((a, b) => a + b, 0) / period;
      prev = seed;
    } else {
      prev = c * k + prev * (1 - k);
    }
    out.push(prev);
  });
  return out;
}

export function computeSma(closes: number[], period: number): (number | null)[] {
  const out: (number | null)[] = [];
  let sum = 0;
  closes.forEach((c, i) => {
    sum += c;
    if (i >= period) sum -= closes[i - period];
    out.push(i >= period - 1 ? sum / period : null);
  });
  return out;
}

/** Bollinger bands: basis = SMA(period), upper/lower = basis ± k·std(period)
 *  (population std over the same window). Returns three parallel arrays aligned
 *  to `closes`, with nulls during the warm-up. */
export function computeBollinger(
  closes: number[],
  period: number,
  k: number,
): { upper: (number | null)[]; basis: (number | null)[]; lower: (number | null)[] } {
  const upper: (number | null)[] = [];
  const basis: (number | null)[] = [];
  const lower: (number | null)[] = [];
  closes.forEach((_, i) => {
    if (i < period - 1) {
      upper.push(null); basis.push(null); lower.push(null);
      return;
    }
    const window = closes.slice(i - period + 1, i + 1);
    const mean = window.reduce((a, b) => a + b, 0) / period;
    const variance = window.reduce((a, b) => a + (b - mean) ** 2, 0) / period;
    const std = Math.sqrt(variance);
    basis.push(mean);
    upper.push(mean + k * std);
    lower.push(mean - k * std);
  });
  return { upper, basis, lower };
}
