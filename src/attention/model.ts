// Pure helpers of the attention screen: what a ticker looks like once the
// universe filter and the after-hours toggle are applied. No React, no canvas.
//
// The after-hours toggle is a DISPLAY choice. The payload always carries both
// sessions and one reference close, so hiding the after-hours re-reads the same
// data — no request, and Y keeps the same zero.

import { nyTime } from "@/lib/nyTime";
import type {
  FrameInfo, Group, MapView, PartStats, Pattern, Pt, TickerView, Universe,
} from "./types";

// ─── Patterns ───────────────────────────────────────────────────────────────────
// Four hues, validated together on the dark surface for colour-blind separation
// (worst pair red↔green ΔE 7.2 deutan, every pair ≥ 16 in normal vision). A fifth
// hue cannot be kept apart from these on a scatter, so « relance » shares the
// green of « hausse tenue » — both are up and still up — and differs by its glyph.
// Every pattern has its own glyph: colour never carries the meaning alone.

export type Glyph = "up" | "steps" | "diamond" | "ring" | "down" | "dot";

export const PATTERNS: Record<Pattern, { label: string; color: string; glyph: Glyph; hint: string }> = {
  held:       { label: "Hausse tenue",       color: "#199e70", glyph: "up",      hint: "Montée rapide, niveau conservé" },
  climb:      { label: "Montée progressive", color: "#3987e5", glyph: "steps",   hint: "Hausse étalée sur plusieurs créneaux" },
  spike_fade: { label: "Pic puis repli",     color: "#c98500", glyph: "diamond", hint: "Au moins la moitié de la hausse rendue" },
  relaunch:   { label: "Relance",            color: "#199e70", glyph: "ring",    hint: "Repli, puis nouvelle poussée" },
  decline:    { label: "Baisse persistante", color: "#d03b3b", glyph: "down",    hint: "Sous la clôture, sans reprise" },
  neutral:    { label: "Neutre",             color: "#898781", glyph: "dot",     hint: "Activité sans mouvement de prix marqué" },
};

export const PATTERN_ORDER: Pattern[] = ["held", "climb", "relaunch", "spike_fade", "decline", "neutral"];

export const INK = {
  surface: "#09090b",
  primary: "#f0efec",
  secondary: "#c3c2b7",
  muted: "#898781",
  grid: "#232325",
  axis: "#383835",
};

// ─── Time ───────────────────────────────────────────────────────────────────────

/** Start of frame slot `i`, epoch seconds. */
export function slotStart(f: FrameInfo, i: number): number {
  return i < f.n_ah
    ? f.ah_start_s + i * f.slot_secs
    : f.pm_start_s + (i - f.n_ah) * f.slot_secs;
}

/** "HH:mm" ET of a slot's start. */
export const slotTime = (f: FrameInfo, i: number) => nyTime(slotStart(f, i));

export const isAh = (f: FrameInfo, i: number) => i < f.n_ah;

// ─── Formatting ─────────────────────────────────────────────────────────────────

export function fmtMoney(v: number | null | undefined): string {
  if (v == null || !Number.isFinite(v)) return "—";
  const a = Math.abs(v);
  if (a >= 1e9) return `${(v / 1e9).toFixed(a >= 1e10 ? 0 : 1)} Md$`;
  if (a >= 1e6) return `${(v / 1e6).toFixed(a >= 1e7 ? 0 : 1)} M$`;
  if (a >= 1e3) return `${(v / 1e3).toFixed(a >= 1e4 ? 0 : 1)} k$`;
  return `${v.toFixed(0)} $`;
}

export function fmtCount(v: number | null | undefined): string {
  if (v == null || !Number.isFinite(v)) return "—";
  const a = Math.abs(v);
  if (a >= 1e9) return `${(v / 1e9).toFixed(1)} Md`;
  if (a >= 1e6) return `${(v / 1e6).toFixed(a >= 1e7 ? 0 : 1)} M`;
  if (a >= 1e3) return `${(v / 1e3).toFixed(a >= 1e4 ? 0 : 1)} k`;
  return v.toFixed(0);
}

export const fmtPct = (v: number | null | undefined, digits = 1) =>
  v == null || !Number.isFinite(v) ? "—" : `${v > 0 ? "+" : ""}${v.toFixed(digits)} %`;

export function fmtRel(v: number | null | undefined): string {
  if (v == null || !Number.isFinite(v)) return "—";
  if (v >= 1000) return `×${(v / 1000).toFixed(1)}k`;
  if (v >= 100) return `×${v.toFixed(0)}`;
  if (v >= 10) return `×${v.toFixed(1)}`;
  return `×${v.toFixed(2)}`;
}

export function fmtPrice(v: number | null | undefined): string {
  if (v == null || !Number.isFinite(v)) return "—";
  return v >= 100 ? v.toFixed(1) : v >= 1 ? v.toFixed(2) : v.toFixed(4);
}

// ─── A ticker as displayed ──────────────────────────────────────────────────────

export interface Shown {
  t: TickerView;
  /** Points of the visible sessions, chronological. */
  pts: Pt[];
  head: Pt;
  pattern: Pattern;
  score: number;
  /** Stats over the visible sessions. */
  hi: number | null;
  lo: number | null;
  vol: number;
  dollars: number;
  xPeak: { x: number; i: number } | null;
  yMax: { y: number; i: number } | null;
  yMin: { y: number; i: number } | null;
  /** Fewest sessions of history behind a visible session's medians. */
  baseN: number;
}

const pick = (
  a: PartStats | null,
  b: PartStats | null,
  val: (s: PartStats) => number | null,
  idx: (s: PartStats) => number | null,
  better: (x: number, y: number) => boolean,
) => {
  let out: { v: number; i: number } | null = null;
  for (const s of [a, b]) {
    if (!s) continue;
    const v = val(s);
    const i = idx(s);
    if (v == null || i == null) continue;
    if (!out || better(v, out.v)) out = { v, i };
  }
  return out;
};

export function shown(t: TickerView, f: FrameInfo, showAh: boolean): Shown | null {
  const pts = showAh ? t.pts : t.pts.filter((p) => p[0] >= f.n_ah);
  if (pts.length === 0) return null;
  const ah = showAh && t.ah.n > 0 ? t.ah : null;
  const pm = t.pm.n > 0 ? t.pm : null;
  const xp = pick(ah, pm, (s) => s.x_peak, (s) => s.x_peak_i, (x, y) => x > y);
  const ymx = pick(ah, pm, (s) => s.y_max, (s) => s.y_max_i, (x, y) => x > y);
  const ymn = pick(ah, pm, (s) => s.y_min, (s) => s.y_min_i, (x, y) => x < y);
  const nums = (vals: (number | null | undefined)[]) =>
    vals.filter((v): v is number => v != null && Number.isFinite(v));
  const his = nums([ah?.hi, pm?.hi]);
  const los = nums([ah?.lo, pm?.lo]);
  return {
    t,
    pts,
    head: pts[pts.length - 1],
    pattern: showAh ? t.pat_all : t.pat_pm,
    score: showAh ? t.score_all : t.score_pm,
    hi: his.length ? Math.max(...his) : null,
    lo: los.length ? Math.min(...los) : null,
    vol: (ah?.vol ?? 0) + (pm?.vol ?? 0),
    dollars: (ah?.dollars ?? 0) + (pm?.dollars ?? 0),
    xPeak: xp && { x: xp.v, i: xp.i },
    yMax: ymx && { y: ymx.v, i: ymx.i },
    yMin: ymn && { y: ymn.v, i: ymn.i },
    baseN: Math.min(...[ah ? t.base_n[0] : null, pm ? t.base_n[1] : null].filter((n): n is number => n != null)),
  };
}

export function inUniverse(group: Group, universe: Universe, includeUnknown: boolean): boolean {
  switch (universe) {
    case "etf":   return group === "etf";
    case "all":   return group !== "etf";
    case "small": return group === "small" || (includeUnknown && group === "unknown");
    case "big":   return group === "big" || (includeUnknown && group === "unknown");
  }
}

/** The tickers to draw, best first. A ticker that only mattered in a hidden
 *  session keeps its place at the end (score 0) rather than vanishing: it is still
 *  on the map, just not the story of the visible session. */
export function visible(
  map: MapView,
  universe: Universe,
  includeUnknown: boolean,
  showAh: boolean,
): Shown[] {
  const out: Shown[] = [];
  for (const t of map.tickers) {
    if (!inUniverse(t.group, universe, includeUnknown)) continue;
    const s = shown(t, map.frame, showAh);
    if (s) out.push(s);
  }
  out.sort((a, b) => b.score - a.score || b.dollars - a.dollars);
  return out;
}

/** Stocks of the map whose cap is unknown — for the chip that flags them. */
export const unknownCount = (map: MapView) =>
  map.tickers.reduce((n, t) => n + (t.group === "unknown" ? 1 : 0), 0);

/** Float rotation of the visible sessions, percent. null = float unknown — never
 *  shown as zero. May exceed 100. */
export function floatRotation(s: Shown): number | null {
  const fl = s.t.float;
  if (fl == null || !(fl > 0)) return null;
  return (s.vol / fl) * 100;
}

// ─── Weather levels ─────────────────────────────────────────────────────────────

export type Level = 0 | 1 | 2 | 3;

/** 0 below `low`, 1 inside the normal band, 2 above `high`, 3 above `extreme`. */
export function level(ratio: number | null, low: number, high: number, extreme = Infinity): Level | null {
  if (ratio == null || !Number.isFinite(ratio)) return null;
  if (ratio < low) return 0;
  if (ratio < high) return 1;
  if (ratio < extreme) return 2;
  return 3;
}
