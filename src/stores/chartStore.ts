import { create } from "zustand";
import type { Timeframe, ZoneTradeContext } from "@/types";

// ─── Public types ─────────────────────────────────────────────────────────────

export type DrawMode = "none" | "line" | "text" | "emoji" | "sl" | "tp" | "alarm";
export type OrderMode = "market" | "limit";
export type LineStyleName = "solid" | "dashed" | "dotted";
export type DrawScope = "intraday" | "daily";

export interface ChartLine {
  id: string;
  point1: { time: number; price: number };
  point2: { time: number; price: number };
  scope: DrawScope;
  color: string;
  opacity: number;       // 0..1
  width: number;         // 1..6
  lineStyle: LineStyleName;
}

export interface ChartAnnotation {
  id: string;
  kind: "text" | "emoji";
  time: number;
  price: number;
  text: string;
  scope: DrawScope;
  color: string;
  opacity: number;       // 0..1
  fontSize: number;      // px
  pixelX: number;
  pixelY: number;
}

export interface ChartAlarm {
  id: string;
  price: number;
}

/** What the right-click context menu is acting on. */
export type CtxTarget =
  | { type: "line"; id: string }
  | { type: "annotation"; id: string }
  | { type: "sl" }
  | { type: "tp" }
  | { type: "alarm"; id: string };

// ─── Per-zone state ────────────────────────────────────────────────────────────

export interface ZoneChartState {
  timeframe:    Timeframe;
  drawMode:     DrawMode;
  orderMode:    OrderMode;
  lines:        ChartLine[];
  annotations:  ChartAnnotation[];
  alarms:       ChartAlarm[];
  linePoint1:   { time: number; price: number } | null;
  /** Glyph chosen in the emoji toolbar, placed on the next click (drawMode="emoji"). */
  pendingEmoji: string | null;
  /** When set, next chart click places a limit order at that price for this % size. */
  pendingLimitPercent: 25 | 50 | 100 | null;
  context:      ZoneTradeContext | null;
}

const DEFAULT_ZONE: ZoneChartState = {
  timeframe:   "1m",
  drawMode:    "none",
  orderMode:   "market",
  lines:       [],
  annotations: [],
  alarms:      [],
  linePoint1:  null,
  pendingEmoji: null,
  pendingLimitPercent: null,
  context:     null,
};

// ─── Store interface ──────────────────────────────────────────────────────────

interface ChartStoreState {
  zones: Record<string, ZoneChartState>;

  getZone:         (zoneId: string) => ZoneChartState;
  setTimeframe:    (zoneId: string, tf: Timeframe) => void;
  setDrawMode:     (zoneId: string, mode: DrawMode) => void;
  setOrderMode:    (zoneId: string, mode: OrderMode) => void;
  setPendingLimitPercent: (zoneId: string, pct: 25 | 50 | 100 | null) => void;
  setLinePoint1:   (zoneId: string, p: { time: number; price: number } | null) => void;
  setPendingEmoji: (zoneId: string, glyph: string | null) => void;
  setLines:        (zoneId: string, lines: ChartLine[]) => void;
  addLine:         (zoneId: string, line: ChartLine) => void;
  updateLine:      (zoneId: string, id: string, patch: Partial<ChartLine>) => void;
  removeLine:      (zoneId: string, id: string) => void;
  setAnnotations:  (zoneId: string, anns: ChartAnnotation[]) => void;
  addAnnotation:   (zoneId: string, ann: ChartAnnotation) => void;
  updateAnnotation:(zoneId: string, id: string, patch: Partial<ChartAnnotation>) => void;
  removeAnnotation:(zoneId: string, id: string) => void;
  setAlarms:       (zoneId: string, alarms: ChartAlarm[]) => void;
  addAlarm:        (zoneId: string, alarm: ChartAlarm) => void;
  removeAlarm:     (zoneId: string, id: string) => void;
  setContext:      (zoneId: string, ctx: ZoneTradeContext | null) => void;
  clearZone:       (zoneId: string) => void;
}

function patch(
  zones: Record<string, ZoneChartState>,
  zoneId: string,
  update: Partial<ZoneChartState>
): Record<string, ZoneChartState> {
  return {
    ...zones,
    [zoneId]: { ...(zones[zoneId] ?? DEFAULT_ZONE), ...update },
  };
}

// ─── Store ────────────────────────────────────────────────────────────────────

export const useChartStore = create<ChartStoreState>((set, get) => ({
  zones: {},

  getZone: (zoneId) => get().zones[zoneId] ?? DEFAULT_ZONE,

  setTimeframe: (zoneId, tf) =>
    set((s) => ({ zones: patch(s.zones, zoneId, { timeframe: tf }) })),

  setDrawMode: (zoneId, mode) =>
    set((s) => ({ zones: patch(s.zones, zoneId, { drawMode: mode, linePoint1: null }) })),

  setOrderMode: (zoneId, mode) =>
    set((s) => ({ zones: patch(s.zones, zoneId, { orderMode: mode, pendingLimitPercent: null }) })),

  setPendingLimitPercent: (zoneId, pct) =>
    set((s) => ({ zones: patch(s.zones, zoneId, { pendingLimitPercent: pct }) })),

  setLinePoint1: (zoneId, p) =>
    set((s) => ({ zones: patch(s.zones, zoneId, { linePoint1: p }) })),

  setPendingEmoji: (zoneId, glyph) =>
    set((s) => ({ zones: patch(s.zones, zoneId, { pendingEmoji: glyph }) })),

  setLines: (zoneId, lines) =>
    set((s) => ({ zones: patch(s.zones, zoneId, { lines }) })),

  addLine: (zoneId, line) =>
    set((s) => {
      const z = s.zones[zoneId] ?? DEFAULT_ZONE;
      return { zones: patch(s.zones, zoneId, { lines: [...z.lines, line] }) };
    }),

  updateLine: (zoneId, id, p) =>
    set((s) => {
      const z = s.zones[zoneId] ?? DEFAULT_ZONE;
      return { zones: patch(s.zones, zoneId, { lines: z.lines.map((l) => l.id === id ? { ...l, ...p } : l) }) };
    }),

  removeLine: (zoneId, id) =>
    set((s) => {
      const z = s.zones[zoneId] ?? DEFAULT_ZONE;
      return { zones: patch(s.zones, zoneId, { lines: z.lines.filter((l) => l.id !== id) }) };
    }),

  setAnnotations: (zoneId, anns) =>
    set((s) => ({ zones: patch(s.zones, zoneId, { annotations: anns }) })),

  addAnnotation: (zoneId, ann) =>
    set((s) => {
      const z = s.zones[zoneId] ?? DEFAULT_ZONE;
      return { zones: patch(s.zones, zoneId, { annotations: [...z.annotations, ann] }) };
    }),

  updateAnnotation: (zoneId, id, p) =>
    set((s) => {
      const z = s.zones[zoneId] ?? DEFAULT_ZONE;
      return { zones: patch(s.zones, zoneId, { annotations: z.annotations.map((a) => a.id === id ? { ...a, ...p } : a) }) };
    }),

  removeAnnotation: (zoneId, id) =>
    set((s) => {
      const z = s.zones[zoneId] ?? DEFAULT_ZONE;
      return { zones: patch(s.zones, zoneId, { annotations: z.annotations.filter((a) => a.id !== id) }) };
    }),

  setAlarms: (zoneId, alarms) =>
    set((s) => ({ zones: patch(s.zones, zoneId, { alarms }) })),

  addAlarm: (zoneId, alarm) =>
    set((s) => {
      const z = s.zones[zoneId] ?? DEFAULT_ZONE;
      return { zones: patch(s.zones, zoneId, { alarms: [...z.alarms, alarm] }) };
    }),

  removeAlarm: (zoneId, id) =>
    set((s) => {
      const z = s.zones[zoneId] ?? DEFAULT_ZONE;
      return { zones: patch(s.zones, zoneId, { alarms: z.alarms.filter((a) => a.id !== id) }) };
    }),

  setContext: (zoneId, ctx) =>
    set((s) => ({ zones: patch(s.zones, zoneId, { context: ctx }) })),

  clearZone: (zoneId) =>
    set((s) => {
      const next = { ...s.zones };
      delete next[zoneId];
      return { zones: next };
    }),
}));
