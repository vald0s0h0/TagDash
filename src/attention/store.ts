import { create } from "zustand";
import { persist } from "zustand/middleware";
import { userStorage } from "@/lib/userStorage";
import type { MapMode, Universe } from "./types";

// What the trader chose on the detached screen. Persisted, so the screen reopens
// as it was left; the selection is not (a ticker of yesterday means nothing today).
interface AttentionPrefs {
  universe: Universe;
  /** Include stocks whose market cap was never collected in small / mid-big. */
  includeUnknown: boolean;
  /** After-hours of the previous session shown. Purely a display choice: the data
   *  is already here and the price reference does not move. */
  showAh: boolean;
  mode: MapMode;
  /** Traces drawn at most; the rest of the map shows as heads only. */
  maxTraces: number;
  /** Follow the trade under « Revue » in the main window. */
  followReview: boolean;
  selected: string | null;
  setUniverse: (u: Universe) => void;
  toggleUnknown: () => void;
  toggleAh: () => void;
  setMode: (m: MapMode) => void;
  setMaxTraces: (n: number) => void;
  setFollowReview: (v: boolean) => void;
  select: (s: string | null) => void;
}

export const useAttentionPrefs = create<AttentionPrefs>()(
  persist(
    (set) => ({
      universe: "small",
      includeUnknown: false,
      showAh: true,
      mode: "traj",
      maxTraces: 40,
      followReview: true,
      selected: null,
      setUniverse: (universe) => set({ universe }),
      toggleUnknown: () => set((s) => ({ includeUnknown: !s.includeUnknown })),
      toggleAh: () => set((s) => ({ showAh: !s.showAh })),
      setMode: (mode) => set({ mode }),
      setMaxTraces: (maxTraces) => set({ maxTraces }),
      setFollowReview: (followReview) => set({ followReview }),
      select: (selected) => set({ selected }),
    }),
    {
      name: "tagdash-attention",
      storage: userStorage(),
      partialize: ({ selected: _selected, ...rest }) => rest,
    },
  ),
);
