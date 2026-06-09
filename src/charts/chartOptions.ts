// Static chart styling/options + small time helpers shared by the chart panes.
// No React and no chart instance — pure data, so they live outside the component.

import { LineStyle, type UTCTimestamp } from "lightweight-charts";
import type { Timeframe } from "@/types";

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
// Bars to fetch per Alpaca call when back-filling (its practical per-request cap).
export const BACKFILL_BATCH = 500;

// SL/TP lines render solid while they are just planned levels, and switch to a
// dotted, lower-opacity "order" style once a position is open (the levels have
// become live bracket orders that will trigger fills when touched).
export function slOpts(order: boolean, title = "SL") {
  return {
    color:            order ? "rgba(239,68,68,0.55)" : "#ef4444",
    lineWidth:        1 as const,
    lineStyle:        order ? LineStyle.Dotted : LineStyle.Solid,
    axisLabelVisible: true,
    title:            order ? `${title} ◦` : title,
  };
}
export function tpOpts(order: boolean, title = "TP") {
  return {
    color:            order ? "rgba(34,197,94,0.55)" : "#22c55e",
    lineWidth:        1 as const,
    lineStyle:        order ? LineStyle.Dotted : LineStyle.Solid,
    axisLabelVisible: true,
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

export const ALARM_OPTIONS = {
  color:            "#f59e0b", // amber
  lineWidth:        1 as const,
  lineStyle:        LineStyle.Dashed,
  axisLabelVisible: true,
  title:            "⏰",
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
