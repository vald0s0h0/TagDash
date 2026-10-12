import { create } from "zustand";

// Playhead of the review replay, shared with the sidebar's context charts (daily +
// 5 min) so they follow the replay without re-rendering the whole sidebar. Written
// by ReviewPlayer at its UI cadence and on every seek.

interface PlayheadState {
  ms: number;
  set: (ms: number) => void;
}

export const usePlayheadStore = create<PlayheadState>((set) => ({
  ms: 0,
  set: (ms) => set({ ms }),
}));
