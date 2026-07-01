import { create } from "zustand";
import { persist } from "zustand/middleware";

// Chart navigation tuning. Persisted to localStorage.
//
// Two independent sensitivity sliders:
//   - `mouseSensitivity` applies when |deltaY| >= 50 (a mouse-wheel notch).
//   - `zoomSensitivity`  applies to the many small-delta events a 2-finger
//     trackpad produces.
//
// `zoomInvert`    — flip zoom-in / zoom-out direction on the time axis.
// `scrollSwapAxes` — swap what vertical vs horizontal 2-finger scroll does:
//   default: vertical → time-axis zoom, horizontal → price-axis zoom.
//   swapped: vertical → price-axis zoom, horizontal → time-axis zoom.

interface ChartInputState {
  mouseSensitivity: number;   // 0.2 .. 4  (mouse wheel)
  zoomSensitivity: number;    // 0.2 .. 4  (2-finger trackpad)
  zoomInvert: boolean;
  scrollSwapAxes: boolean;
  setMouseSensitivity: (v: number) => void;
  setZoomSensitivity: (v: number) => void;
  setZoomInvert: (v: boolean) => void;
  setScrollSwapAxes: (v: boolean) => void;
}

export const SENS_MIN = 0.2;
export const SENS_MAX = 4;

const clamp = (v: number) => Math.max(SENS_MIN, Math.min(SENS_MAX, v));

export const useChartInput = create<ChartInputState>()(
  persist(
    (set) => ({
      mouseSensitivity: 1,
      zoomSensitivity: 1,
      zoomInvert: false,
      scrollSwapAxes: false,
      setMouseSensitivity: (v) => set({ mouseSensitivity: clamp(v) }),
      setZoomSensitivity:  (v) => set({ zoomSensitivity: clamp(v) }),
      setZoomInvert:       (v) => set({ zoomInvert: v }),
      setScrollSwapAxes:   (v) => set({ scrollSwapAxes: v }),
    }),
    { name: "tagdash-chart-input" },
  ),
);
