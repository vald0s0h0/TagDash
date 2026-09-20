// Static chart styling/options + small time helpers shared by the chart panes.
// No React and no chart instance — pure data, so they live outside the component.

import { LineStyle, type UTCTimestamp } from "lightweight-charts";
import type { Timeframe } from "@/types";
import { getChartTheme } from "@/stores/chartThemeStore";
import { hexToRgba } from "@/charts/drawingsPrimitive";

export function toUTC(isoString: string): UTCTimestamp {
  return Math.floor(new Date(isoString).getTime() / 1000) as UTCTimestamp;
}

/** Bucket size (seconds) per timeframe — for placing a live tick on its candle. */
export const TF_SECONDS: Record<Timeframe, number> = {
  "5s": 5, "10s": 10, "1m": 60, "2m": 120, "5m": 300, "15m": 900, "daily": 86400,
};

// Lazy back-fill kicks in when the visible left edge gets within this many bars of
// the first loaded bar (logical index). Generous so older history is fetched a bit
// BEFORE the user actually scrolls onto the blank, avoiding a visible gap.
export const BACKFILL_THRESHOLD = 100;
// Older bars pulled per back-fill step (one Alpaca call per trigger). The chart's
// visible-range handler re-fires as the user keeps scrolling left, so history grows
// in steps of this size — the official lightweight-charts "infinite history" pattern.
export const BACKFILL_BATCH = 300;

// SL/TP lines render solid while they are just planned levels, and switch to a
// dotted, lower-opacity "order" style once a position is open (the levels have
// become live bracket orders that will trigger fills when touched).
//
// `axisLabelVisible` is false on purpose: the right-edge "price + ✕" label pill
// (rendered in the component, tracked to the line) replaces the native axis tag,
// so the delete button is part of the label instead of a separate overlay.
export function slOpts(order: boolean, title = "SL") {
  const c = getChartTheme().levels.sl;
  return {
    color:            order ? hexToRgba(c, 0.55) : c,
    lineWidth:        1 as const,
    lineStyle:        order ? LineStyle.Dotted : LineStyle.Solid,
    axisLabelVisible: false,
    title:            order ? `${title} ◦` : title,
  };
}
export function tpOpts(order: boolean, title = "TP") {
  const c = getChartTheme().levels.tp;
  return {
    color:            order ? hexToRgba(c, 0.55) : c,
    lineWidth:        1 as const,
    lineStyle:        order ? LineStyle.Dotted : LineStyle.Solid,
    axisLabelVisible: false,
    title:            order ? `${title} ◦` : title,
  };
}

export function limitOpts(side: "long" | "short" | null) {
  const c = getChartTheme().levels.limit;
  return {
    color:            hexToRgba(c, 0.55),
    lineWidth:        1 as const,
    lineStyle:        LineStyle.Dotted,
    axisLabelVisible: false,
    title:            side === "long" ? "Limit ▲" : side === "short" ? "Limit ▼" : "Limit",
  };
}

export const ENTRY_OPTIONS = {
  color:            "rgba(250,250,250,0.25)",
  lineWidth:        1 as const,
  lineStyle:        LineStyle.Dashed,
  axisLabelVisible: true,
  title:            "Entry",
};

// Draft (suggested) trade lines — faint orange, fine dotted.
const DRAFT_COLOR = "rgba(245,158,11,0.35)";
export const DRAFT_ENTRY_OPTS = {
  color: DRAFT_COLOR, lineWidth: 1 as const, lineStyle: LineStyle.Dotted,
  axisLabelVisible: false, title: "Entry ⟡",
};
export const DRAFT_SL_OPTS = {
  color: DRAFT_COLOR, lineWidth: 1 as const, lineStyle: LineStyle.Dotted,
  axisLabelVisible: false, title: "SL ⟡",
};
export const DRAFT_TP_OPTS = {
  color: DRAFT_COLOR, lineWidth: 1 as const, lineStyle: LineStyle.Dotted,
  axisLabelVisible: false, title: "TP ⟡",
};

// Armed Apex — violet dashed lines riding the price at the bracket distances. Loud
// on purpose: while they are on screen a market order can leave at any print.
// Dimmed while a distance is under the minimum (armed, but nothing fires).
//
// With both directions armed there can be four levels, because the stop and the
// target no longer share a distance: above the price sit the long's target and the
// short's stop, below them the long's stop and the short's target. Levels that land
// on the same price are drawn once. Under the dynamic take profit there is no target
// price at all, so only the stops are drawn.
const APEX_COLOR = "#c084fc";

function apexDistanceLabel(d: number): string {
  return d >= 0.01 ? d.toFixed(2) : d.toFixed(4);
}

export interface ApexLineDef {
  /** Stable key so the chart can move a line instead of recreating it. */
  key: string;
  /** Signed offset from the current price. */
  offset: number;
  title: string;
}

/** The lines an armed Apex draws, top to bottom. */
export function apexLineDefs(spec: {
  slDistance: number;
  tpDistance: number | null;
  long: boolean;
  short: boolean;
  tradeable: boolean;
}): ApexLineDef[] {
  const { slDistance: sl, tpDistance: tp } = spec;
  const raw: { offset: number; kind: "tp" | "sl" }[] = [];
  if (spec.long) {
    if (tp != null) raw.push({ offset: tp, kind: "tp" });
    raw.push({ offset: -sl, kind: "sl" });
  }
  if (spec.short) {
    raw.push({ offset: sl, kind: "sl" });
    if (tp != null) raw.push({ offset: -tp, kind: "tp" });
  }
  // One line per price level, whatever number of roles meet there.
  const byOffset = new Map<string, { offset: number; kinds: Set<"tp" | "sl"> }>();
  for (const r of raw) {
    const key = r.offset.toFixed(6);
    const entry = byOffset.get(key) ?? { offset: r.offset, kinds: new Set<"tp" | "sl">() };
    entry.kinds.add(r.kind);
    byOffset.set(key, entry);
  }
  const suffix = (spec.tradeable ? "" : " · < min") + (tp == null ? " · TP dyn" : "");
  return [...byOffset.values()]
    .sort((a, b) => b.offset - a.offset)
    .map(({ offset, kinds }) => {
      const sign = offset >= 0 ? "+" : "−";
      const amount = apexDistanceLabel(Math.abs(offset));
      const role = kinds.size === 2 ? (offset >= 0 ? "▲" : "▼") : kinds.has("tp") ? "TP" : "SL";
      return {
        key: `${offset >= 0 ? "up" : "down"}:${amount}`,
        offset,
        title: `Apex ${role} ${sign}${amount}${suffix}`,
      };
    });
}

/** Styling shared by every Apex line. */
export function apexLineStyle(tradeable: boolean) {
  return {
    color: tradeable ? APEX_COLOR : hexToRgba(APEX_COLOR, 0.35),
    lineWidth: 2 as const,
    lineStyle: LineStyle.Dashed,
    axisLabelVisible: true,
  };
}

// Live bid / ask pair. Neither line is named any more: the ask is always the upper
// of the two, so the words only ate pixels. The ask keeps the native axis tag (its
// price); the bid has its tag switched off and gets a right-edge SPREAD pill in its
// place (rendered by the component, like the SL/TP pills) — so one glance reads
// "price on top, cost of crossing underneath" instead of two prices the eye has to
// subtract. The pill is DOM because a native price line can only ever print its own
// price, and its in-pane `title` is gated behind the same `axisLabelVisible` flag.
export const BID_ASK_OPTIONS = {
  color:            "rgba(200,200,200,0.45)",
  lineWidth:        1 as const,
  lineStyle:        LineStyle.Dashed,
  axisLabelVisible: true,
};

/** Colour of the bid's spread pill — the bid/ask line grey, lifted enough to read
 *  as text against the chart background. */
export const SPREAD_TAG_COLOR = "rgba(210,210,210,0.85)";

/** The bid line's label: the spread, not a price. Sub-cent spreads keep four
 *  decimals — on a $0.40 name `0.00` would read as "no spread at all", which is
 *  exactly the wrong call to make before crossing it. */
export function spreadLabel(bid: number, ask: number): string {
  const d = Math.max(ask - bid, 0);
  return d >= 0.01 ? d.toFixed(2) : d.toFixed(4);
}

/** Price-alarm line style — amber by default, colour from the theme. A function
 *  (not a constant) so the colour is read live at create-time. */
export function alarmOpts() {
  return {
    color:            getChartTheme().levels.alarm,
    lineWidth:        1 as const,
    lineStyle:        LineStyle.Dashed,
    // Native axis tag off — the right-edge "price + ✕" label pill stands in for it.
    axisLabelVisible: false,
    title:            "⏰",
  };
}

// Controller horizontal cursor (right stick). A distinct sky-blue dashed line the
// pad nudges up/down; A/B/Y drop an SL/alarm/TP at its price. The axis tag stays
// on (so the exact price is readable while aiming) since it isn't a persisted level.
export const CURSOR_OPTIONS = {
  color:            "#38bdf8", // sky
  lineWidth:        1 as const,
  lineStyle:        LineStyle.Dashed,
  axisLabelVisible: true,
  title:            "⊹",
};

// Previous-day reference levels (close = key, high/low = secondary). Drawn as
// horizontal price lines from the previous trading day's daily bar.
export const PREV_DAY_OPTIONS: Record<"previous_close" | "previous_high" | "previous_low", {
  color: string; lineStyle: LineStyle; title: string;
}> = {
  previous_close: { color: "#eab308",            lineStyle: LineStyle.Dashed, title: "PDC" }, // yellow, solidish
  previous_high:  { color: "rgba(148,163,184,0.6)", lineStyle: LineStyle.Dotted, title: "PDH" },
  previous_low:   { color: "rgba(148,163,184,0.6)", lineStyle: LineStyle.Dotted, title: "PDL" },
};
