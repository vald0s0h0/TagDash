import { create } from "zustand";
import { persist } from "zustand/middleware";

// Trackpad / wheel tuning for the chart's X (time) axis zoom. A 2-finger trackpad
// scroll drives the time-axis zoom (the current bar stays pinned at the default
// view, as usual) — these let the user scale how fast it zooms and flip the
// direction (e.g. to match macOS "natural" scrolling). Persisted to localStorage.
//
// `zoomSensitivity` 1 = default: a mouse-wheel notch (~100 px delta) keeps the
// classic ~1.1× step; a trackpad's many small-delta events stay smooth. Higher =
// faster zoom.

interface ChartInputState {
  zoomSensitivity: number; // 0.2 .. 4
  zoomInvert: boolean;
  setZoomSensitivity: (v: number) => void;
  setZoomInvert: (v: boolean) => void;
}

export const SENS_MIN = 0.2;
export const SENS_MAX = 4;

export const useChartInput = create<ChartInputState>()(
  persist(
    (set) => ({
      zoomSensitivity: 1,
      zoomInvert: false,
      setZoomSensitivity: (v) =>
        set({ zoomSensitivity: Math.max(SENS_MIN, Math.min(SENS_MAX, v)) }),
      setZoomInvert: (v) => set({ zoomInvert: v }),
    }),
    { name: "tagdash-chart-input" },
  ),
);
