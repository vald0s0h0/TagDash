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

export const ENTRY_OPTIONS = {
  color:            "rgba(250,250,250,0.25)",
  lineWidth:        1 as const,
  lineStyle:        LineStyle.Dashed,
  axisLabelVisible: true,
  title:            "Entry",
};

export const BID_ASK_OPTIONS = {
  color:            "rgba(200,200,200,0.45)",
  lineWidth:        1 as const,
  lineStyle:        LineStyle.Dashed,
  axisLabelVisible: true,
};

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
