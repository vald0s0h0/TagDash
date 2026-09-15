import { create } from "zustand";
import { persist } from "zustand/middleware";

// Chart navigation tuning. Persisted to localStorage.
//
// Four independent sensitivity sliders, one per (device × physical wheel axis):
//   - `mouseSensitivityX` / `mouseSensitivityY`       — a mouse wheel notch
//     (|delta| >= 50), horizontal vs vertical.
//   - `trackpadSensitivityX` / `trackpadSensitivityY` — the many small-delta
//     events a 2-finger trackpad scroll produces, horizontal vs vertical.
// Which one applies depends only on which physical wheel moved (X vs Y) and
// how big the delta was (mouse vs trackpad) — NOT on what that wheel controls
// (price-axis zoom or time-axis zoom), since `scrollSwapAxes` below remaps
// that separately.
//
// `zoomInvert`    — flip zoom-in / zoom-out direction on the time axis.
// `scrollSwapAxes` — swap what vertical vs horizontal scroll does:
//   default: vertical → time-axis zoom, horizontal → price-axis zoom.
//   swapped: vertical → price-axis zoom, horizontal → time-axis zoom.

interface ChartInputState {
  mouseSensitivityX: number;    // 0.2 .. 4  (mouse wheel, horizontal)
  mouseSensitivityY: number;    // 0.2 .. 4  (mouse wheel, vertical)
  trackpadSensitivityX: number; // 0.2 .. 4  (2-finger trackpad, horizontal)
  trackpadSensitivityY: number; // 0.2 .. 4  (2-finger trackpad, vertical)
  zoomInvert: boolean;
  scrollSwapAxes: boolean;
  setMouseSensitivityX: (v: number) => void;
  setMouseSensitivityY: (v: number) => void;
  setTrackpadSensitivityX: (v: number) => void;
  setTrackpadSensitivityY: (v: number) => void;
  setZoomInvert: (v: boolean) => void;
  setScrollSwapAxes: (v: boolean) => void;
}

export const SENS_MIN = 0.2;
export const SENS_MAX = 4;

const clamp = (v: number) => Math.max(SENS_MIN, Math.min(SENS_MAX, v));

export const useChartInput = create<ChartInputState>()(
  persist(
    (set) => ({
      mouseSensitivityX: 1,
      mouseSensitivityY: 1,
      trackpadSensitivityX: 1,
      trackpadSensitivityY: 1,
      zoomInvert: false,
      scrollSwapAxes: false,
      setMouseSensitivityX:    (v) => set({ mouseSensitivityX: clamp(v) }),
      setMouseSensitivityY:    (v) => set({ mouseSensitivityY: clamp(v) }),
      setTrackpadSensitivityX: (v) => set({ trackpadSensitivityX: clamp(v) }),
      setTrackpadSensitivityY: (v) => set({ trackpadSensitivityY: clamp(v) }),
      setZoomInvert:           (v) => set({ zoomInvert: v }),
      setScrollSwapAxes:       (v) => set({ scrollSwapAxes: v }),
    }),
    { name: "tagdash-chart-input" },
  ),
);
