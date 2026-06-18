import { create } from "zustand";

/** Per-ticker "released" flag driving the alert card's state (with the active
 *  chart symbol + the alert age). A symbol is `released` once the user frees it
 *  (Libérer, on the chart or the card's bird button) and stops being released the
 *  moment it's shown again.
 *
 *  Card state (derived in ScannerAlerts):
 *   - symbol === active chart      → active   (red bar, high shade)
 *   - released                     → idle     (grey bar, low shade) — never blinks
 *   - age < WAITING_MS, otherwise  → waiting  (blinking red bar + low blinking shade)
 *   - otherwise                    → idle     (grey bar, low shade)
 *
 *  It also gates the "open the most recent pending ticker on release" behaviour:
 *  released symbols are excluded from that auto-open.
 *
 *  Ephemeral (RAM only): resets on relaunch, which is fine — the chart zones also
 *  start empty, so nothing is stale. */
interface AlertStatusState {
  released: Set<string>;
  /** A symbol shown in the chart is no longer "released". */
  markObserved: (symbol: string) => void;
  /** Mark a symbol released (Libérer). */
  markReleased: (symbol: string) => void;
}

export const useAlertStatusStore = create<AlertStatusState>((set) => ({
  released: new Set<string>(),

  markObserved: (symbol) =>
    set((s) => {
      if (!s.released.has(symbol)) return {};
      const released = new Set(s.released);
      released.delete(symbol);
      return { released };
    }),

  markReleased: (symbol) =>
    set((s) => {
      if (s.released.has(symbol)) return {};
      const released = new Set(s.released);
      released.add(symbol);
      return { released };
    }),
}));
