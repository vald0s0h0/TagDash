import { create } from "zustand";
import type { Timeframe, ZoneTradeContext } from "@/types";

// ─── Public types ─────────────────────────────────────────────────────────────

export type DrawMode = "none" | "line" | "text" | "sl" | "tp" | "alarm";
export type OrderMode = "market" | "limit";

export interface ChartLine {
  id: string;
  point1: { time: number; price: number };
  point2: { time: number; price: number };
}

export interface ChartAnnotation {
  id: string;
  time: number;
  price: number;
  text: string;
  pixelX: number;
  pixelY: number;
}

export interface ChartAlarm {
  id: string;
  price: number;
}

// ─── Per-zone state ────────────────────────────────────────────────────────────

export interface ZoneChartState {
  timeframe:    Timeframe;
  drawMode:     DrawMode;
  orderMode:    OrderMode;
  lines:        ChartLine[];
  annotations:  ChartAnnotation[];
  alarms:       ChartAlarm[];
  linePoint1:   { time: number; price: number } | null;
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
  context:     null,
};

// ─── Store interface ──────────────────────────────────────────────────────────

interface ChartStoreState {
  zones: Record<string, ZoneChartState>;

  getZone:         (zoneId: string) => ZoneChartState;
  setTimeframe:    (zoneId: string, tf: Timeframe) => void;
  setDrawMode:     (zoneId: string, mode: DrawMode) => void;
  setOrderMode:    (zoneId: string, mode: OrderMode) => void;
  setLinePoint1:   (zoneId: string, p: { time: number; price: number } | null) => void;
  setLines:        (zoneId: string, lines: ChartLine[]) => void;
  addLine:         (zoneId: string, line: ChartLine) => void;
  setAnnotations:  (zoneId: string, anns: ChartAnnotation[]) => void;
  addAnnotation:   (zoneId: string, ann: ChartAnnotation) => void;
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
    set((s) => ({ zones: patch(s.zones, zoneId, { orderMode: mode }) })),

  setLinePoint1: (zoneId, p) =>
    set((s) => ({ zones: patch(s.zones, zoneId, { linePoint1: p }) })),

  setLines: (zoneId, lines) =>
    set((s) => ({ zones: patch(s.zones, zoneId, { lines }) })),

  addLine: (zoneId, line) =>
    set((s) => {
      const z = s.zones[zoneId] ?? DEFAULT_ZONE;
      return { zones: patch(s.zones, zoneId, { lines: [...z.lines, line] }) };
    }),

  setAnnotations: (zoneId, anns) =>
    set((s) => ({ zones: patch(s.zones, zoneId, { annotations: anns }) })),

  addAnnotation: (zoneId, ann) =>
    set((s) => {
      const z = s.zones[zoneId] ?? DEFAULT_ZONE;
      return { zones: patch(s.zones, zoneId, { annotations: [...z.annotations, ann] }) };
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
