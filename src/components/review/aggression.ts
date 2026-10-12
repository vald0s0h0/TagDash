// « Tape Agression » — how hard buyers and sellers hit the book, print by print.
//
// Each print gets a side and a weight:
//   • side: lifted the ask (buyer) or hit the bid (seller), against the NBBO at
//     that instant; a print exactly at the mid, or with no quote at all, falls back
//     on the tick rule;
//   • weight = price aggression × size: how far through the quote it traded (0.5 at
//     the mid, 1.5 on the quote, more when it swept beyond it) × its size relative
//     to the session's median print.
// The weights feed one exponentially decaying rate per side (τ = 350 ms of market
// time): the intensity at any instant, a pure function of the playhead.
//
// Colours are relative to the session itself (whatever the timeframe shown): the
// rank of an intensity among the intensities reached at each print of the recorded
// tape takes a band from its dark base to its colour (a typical print), then light,
// then white (top 3 %), fading back as the rate decays. Violet is kept for the
// exceptional: beyond both the top 0.5 % and twice the white threshold.

import type { ReviewSessionData } from "@/types";

export const TAU_MS = 350;
/** Past this, the decayed rate counts as silence (≈ 0.3 % of the last value). */
const SILENT_MS = 6 * TAU_MS;
/** Cap on the intensities kept to rank against (long sessions sample coarser). */
const MAX_SAMPLES = 100_000;

export interface SideRate {
  /** Print times (ms) and the decayed rate just after each print. */
  t: Float64Array;
  e: Float64Array;
}

export interface Aggression {
  buy: SideRate;
  sell: SideRate;
  /** Intensities reached at the prints (both sides), ascending. */
  sorted: Float64Array;
  white: number;
  violet: number;
}

function median(values: number[]): number {
  if (!values.length) return 1;
  const s = [...values].sort((a, b) => a - b);
  return s[s.length >> 1] || 1;
}

function lastAtOrBefore(arr: Float64Array, x: number): number {
  let lo = 0;
  let hi = arr.length - 1;
  let ans = -1;
  while (lo <= hi) {
    const mid = (lo + hi) >> 1;
    if (arr[mid] <= x) {
      ans = mid;
      lo = mid + 1;
    } else hi = mid - 1;
  }
  return ans;
}

/** Decayed rate of one side at `t` (0 when silent). */
export function intensity(side: SideRate, t: number): number {
  const i = lastAtOrBefore(side.t, t);
  if (i < 0) return 0;
  const dt = t - side.t[i];
  return dt > SILENT_MS ? 0 : side.e[i] * Math.exp(-dt / TAU_MS);
}

export function buildAggression(data: ReviewSessionData): Aggression {
  const n = data.trades_ts.length;
  const sizes: number[] = [];
  const steps: number[] = [];
  for (let i = 0; i < n; i++) {
    if (data.trades_size[i] > 0) sizes.push(data.trades_size[i]);
    if (i > 0) {
      const d = Math.abs(data.trades_price[i] - data.trades_price[i - 1]);
      if (d > 0) steps.push(d);
    }
  }
  const medSize = median(sizes);
  const medStep = Math.max(0.0001, median(steps));

  const bt: number[] = [];
  const be: number[] = [];
  const st: number[] = [];
  const se: number[] = [];
  const push = (t: number[], e: number[], ms: number, w: number) => {
    const k = t.length;
    const prev = k ? e[k - 1] * Math.exp(-(ms - t[k - 1]) / TAU_MS) : 0;
    t.push(ms);
    e.push(prev + w / (TAU_MS / 1000));
  };

  let q = -1;
  let lastSide = 0;
  for (let i = 0; i < n; i++) {
    const ms = data.trades_ts[i];
    const p = data.trades_price[i];
    while (q + 1 < data.quotes_ts.length && data.quotes_ts[q + 1] <= ms) q++;
    const bid = q >= 0 ? data.quotes_bid[q] : 0;
    const ask = q >= 0 ? data.quotes_ask[q] : 0;
    const tick = i > 0 ? Math.sign(p - data.trades_price[i - 1]) : 0;
    let side: number;
    let through: number;
    if (bid > 0 && ask >= bid) {
      const mid = (bid + ask) / 2;
      const d = (p - mid) / Math.max((ask - bid) / 2, 0.005);
      side = d > 0 ? 1 : d < 0 ? -1 : tick || lastSide;
      through = Math.abs(d);
    } else {
      side = tick || lastSide;
      through = 1 + Math.min(3, (i > 0 ? Math.abs(p - data.trades_price[i - 1]) : 0) / medStep);
    }
    if (!side) continue;
    lastSide = side;
    const w = (0.5 + Math.min(through, 5)) * Math.min(60, data.trades_size[i] / medSize);
    if (side > 0) push(bt, be, ms, w);
    else push(st, se, ms, w);
  }
  const buy = { t: Float64Array.from(bt), e: Float64Array.from(be) };
  const sell = { t: Float64Array.from(st), e: Float64Array.from(se) };

  const all = be.length + se.length;
  const stride = Math.max(1, Math.ceil(all / MAX_SAMPLES));
  const samples: number[] = [];
  for (let i = 0; i < all; i += stride) samples.push(i < be.length ? be[i] : se[i - be.length]);
  const sorted = Float64Array.from(samples).sort();
  const quant = (p: number) => (sorted.length ? sorted[Math.min(sorted.length - 1, Math.floor(p * sorted.length))] : 1);
  const white = quant(0.97);
  const violet = Math.max(quant(0.995), 2 * white);
  return { buy, sell, sorted, white, violet };
}

// ─── Colours ──────────────────────────────────────────────────────────────────

type Rgb = [number, number, number];
const WHITE: Rgb = [255, 255, 255];
const PALE_VIOLET: Rgb = [233, 213, 255];
const VIOLET: Rgb = [168, 85, 247];
const BANDS: Record<"buy" | "sell", { base: Rgb; stops: [number, Rgb][] }> = {
  buy: {
    base: [10, 36, 21],
    stops: [[0, [10, 36, 21]], [0.55, [22, 163, 74]], [0.85, [134, 239, 172]], [1, WHITE]],
  },
  sell: {
    base: [42, 12, 14],
    stops: [[0, [42, 12, 14]], [0.55, [220, 38, 38]], [0.85, [252, 165, 165]], [1, WHITE]],
  },
};

const css = ([r, g, b]: Rgb) => `rgb(${r | 0},${g | 0},${b | 0})`;
const mix = (a: Rgb, b: Rgb, u: number): Rgb => [a[0] + (b[0] - a[0]) * u, a[1] + (b[1] - a[1]) * u, a[2] + (b[2] - a[2]) * u];

const LUT_STEPS = 128;
const luts = (["buy", "sell"] as const).reduce(
  (acc, side) => {
    const { stops } = BANDS[side];
    acc[side] = Array.from({ length: LUT_STEPS + 1 }, (_, k) => {
      const u = k / LUT_STEPS;
      let j = 1;
      while (j < stops.length - 1 && u > stops[j][0]) j++;
      const [u0, c0] = stops[j - 1];
      const [u1, c1] = stops[j];
      return css(mix(c0, c1, (u - u0) / Math.max(1e-9, u1 - u0)));
    });
    return acc;
  },
  {} as Record<"buy" | "sell", string[]>,
);
const baseCss = { buy: css(BANDS.buy.base), sell: css(BANDS.sell.base) };
const PALE_STEPS = 16;
const paleLut = Array.from({ length: PALE_STEPS + 1 }, (_, k) => css(mix(WHITE, PALE_VIOLET, k / PALE_STEPS)));
const violetCss = css(VIOLET);

/** Colour of an intensity on its band. */
export function aggressionColor(agg: Aggression, side: "buy" | "sell", v: number): string {
  if (v <= 0 || !agg.sorted.length) return baseCss[side];
  if (v >= agg.violet) return violetCss;
  if (v >= agg.white) {
    const u = Math.log(v / agg.white) / Math.log(agg.violet / agg.white);
    return paleLut[Math.round(Math.min(1, Math.max(0, u)) * PALE_STEPS)];
  }
  // Mid-rank among the print intensities (ties share the middle), rescaled so the
  // white threshold (97th) is 1.
  const below = (x: number, strict: boolean) => {
    let lo = 0;
    let hi = agg.sorted.length;
    while (lo < hi) {
      const mid = (lo + hi) >> 1;
      if (strict ? agg.sorted[mid] < x : agg.sorted[mid] <= x) lo = mid + 1;
      else hi = mid;
    }
    return lo;
  };
  const rank = (below(v, true) + below(v, false)) / 2;
  const u = Math.min(1, rank / agg.sorted.length / 0.97);
  return luts[side][Math.round(u * LUT_STEPS)];
}
