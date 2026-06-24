// Module-level registries wiring the global gamepad loop (useGamepad) to the
// mounted chart panes and chart zones — same pattern as `registerZoneHotkeys` /
// `crosshairSync`. No React, no re-renders: the loop reads these every frame.
//
// Two registries because the controller has two kinds of target:
//   • analog sticks act on ONE pane (the focused chart) → `chartControl` keyed by paneId.
//   • digital buttons act on the WHOLE zone (orders, capture, focus) → `zoneGamepad` keyed by zoneId.

// ─── Per-pane chart controls (analog sticks: zoom + horizontal cursor) ─────────

export interface ChartControl {
  /** Zoom the time axis (main wheel equivalent). factor>1 = zoom in. */
  zoomTime: (factor: number) => void;
  /** Zoom the price axis (2nd wheel equivalent). step>0 = zoom out (looser). */
  zoomPrice: (step: number) => void;
  /** Move the horizontal price cursor by a fraction of the visible price range.
   *  Seeds at the current price (last bar close) on the first nudge. */
  nudgeCursor: (deltaFrac: number) => void;
  /** Current cursor price (null if never placed for this symbol). */
  getCursorPrice: () => number | null;
  /** Remove the cursor line (e.g. on focus change). */
  clearCursor: () => void;
}

const chartControls = new Map<string, ChartControl>();

export function registerChartControl(paneId: string, ctl: ChartControl): () => void {
  chartControls.set(paneId, ctl);
  return () => { if (chartControls.get(paneId) === ctl) chartControls.delete(paneId); };
}

export function getChartControl(paneId: string | null): ChartControl | undefined {
  return paneId ? chartControls.get(paneId) : undefined;
}

// ─── Per-zone gamepad handlers (digital buttons) ──────────────────────────────

export interface ZoneGamepad {
  /** paneId the sticks should drive (default = interactive pane; cycled by R1). */
  getFocusedPaneId: () => string;
  /** R1: move focus to the next pane of the zone (wraps; no-op if single pane). */
  cycleFocus: () => void;
  /** Cursor/order layer (R2 not held). */
  placeSl:    () => void;
  placeTp:    () => void;
  placeAlarm: () => void;
  removeOrders: () => void;
  removeOrdersAndAlarms: () => void;
  /** Armed layer (R2 held). */
  order: (percent: 25 | 50 | 100) => void;
  close: () => void;
  /** TradeTally capture (View button). */
  capture: () => void;
  /** Release the zone (D-pad left, non-screener). */
  release: () => void;
  /** Whether the zone currently carries a live tradeID (gates capture / share). */
  hasTradeId: () => boolean;
  /** The zone's current tradeID, or null (used by the auto-tag flow). */
  tradeId: () => string | null;
  /** The zone's displayed symbol, or null. */
  symbol: () => string | null;
}

const zoneGamepads = new Map<string, ZoneGamepad>();

export function registerZoneGamepad(zoneId: string, handlers: ZoneGamepad): () => void {
  zoneGamepads.set(zoneId, handlers);
  return () => { if (zoneGamepads.get(zoneId) === handlers) zoneGamepads.delete(zoneId); };
}

export function getZoneGamepad(zoneId: string | null): ZoneGamepad | undefined {
  return zoneId ? zoneGamepads.get(zoneId) : undefined;
}

// ─── Recorder lock ────────────────────────────────────────────────────────────
// Set while the Settings binding recorder is reading the pad, so the global loop
// doesn't fire an action for the very button/axis being assigned (mirrors the
// keyboard recorder's `setRecordingActive`).

let capturing = false;
export function setGamepadCapturing(v: boolean): void { capturing = v; }
export function isGamepadCapturing(): boolean { return capturing; }
