// Pure indicator math + styling constants for chart panes. No React, no chart
// instance — just functions over close-price arrays, so they're trivially
// testable and reusable across panes (and future option charts).

import type { PaneIndicator } from "@/types";

export const INDICATOR_COLORS = {
  vwap: "#3b82f6", // blue
  ema:  "#60a5fa", // light blue
  sma:  "#9ca3af", // grey
} as const;

/** Bollinger period multiplier (classic 2σ). */
export const BOLLINGER_K = 2;
export const BOLLINGER_COLORS = { band: "#a78bfa", basis: "rgba(167,139,250,0.5)" }; // violet

/** Stable id for an indicator so series can be created/updated/removed in place. */
export function indicatorId(ind: PaneIndicator): string {
  return ind.period != null ? `${ind.kind}-${ind.period}` : ind.kind;
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
