import type { Time } from "lightweight-charts";

// Global (cross-pane) crosshair sync. Each LightweightChart pane registers a
// target; when the user hovers one pane, that pane broadcasts the hovered
// (time, price) and every OTHER registered pane of the SAME instrument mirrors
// the crosshair via the official `setCrosshairPosition` / `clearCrosshairPosition`
// API (see lightweight-charts docs, "Set crosshair position"). One group is
// created per ChartZone so panes of one zone stay in sync without leaking across
// zones.

export interface CrosshairTarget {
  /** Instrument shown by this pane — only same-symbol panes mirror each other. */
  symbol: string;
  /** Draw the synced crosshair at (time, price) on this pane. */
  apply: (time: Time, price: number) => void;
  /** Remove the synced crosshair from this pane. */
  clear: () => void;
}

export interface CrosshairSync {
  /** Register a pane; returns an unregister fn for cleanup. */
  register: (id: string, target: CrosshairTarget) => () => void;
  /** Broadcast a hover (or a leave, when time/price are null) from one pane. */
  broadcast: (sourceId: string, symbol: string, time: Time | null, price: number | null) => void;
  /** True while a programmatic mirror is in progress — lets pane move handlers
   *  ignore the echo and avoid a feedback loop. */
  syncing: boolean;
}

export function createCrosshairSync(): CrosshairSync {
  const targets = new Map<string, CrosshairTarget>();
  const group: CrosshairSync = {
    syncing: false,
    register(id, target) {
      targets.set(id, target);
      return () => { targets.delete(id); };
    },
    broadcast(sourceId, symbol, time, price) {
      group.syncing = true;
      for (const [id, t] of targets) {
        if (id === sourceId || t.symbol !== symbol) continue;
        if (time != null && price != null) t.apply(time, price);
        else t.clear();
      }
      group.syncing = false;
    },
  };
  return group;
}
