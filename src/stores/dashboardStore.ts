import { create } from "zustand";
import { persist } from "zustand/middleware";

// The dashboard grid is a fixed, invisible 10×6 board. Cards are placed and sized
// in whole grid cells (0-indexed x/y, 1-based span via CSS grid). Layout +
// visibility persist to localStorage so the user's arrangement survives restarts.

export const GRID_COLS = 10;
export const GRID_ROWS = 6;

/** Built-in card ids. Add new cards here + in `components/dashboard/cards.tsx`
 *  and give them a default slot in DEFAULT_LAYOUT. */
export type CardId = "kpis" | "pnl-curve" | "rolling-pf" | "journal";

export interface CardLayout {
  x: number;
  y: number;
  w: number;
  h: number;
  visible: boolean;
}

export const DEFAULT_LAYOUT: Record<CardId, CardLayout> = {
  "kpis":       { x: 0, y: 0, w: 4, h: 2, visible: true },
  "journal":    { x: 0, y: 2, w: 4, h: 4, visible: true },
  "pnl-curve":  { x: 4, y: 0, w: 6, h: 3, visible: true },
  "rolling-pf": { x: 4, y: 3, w: 6, h: 3, visible: true },
};

function cloneDefault(): Record<CardId, CardLayout> {
  return JSON.parse(JSON.stringify(DEFAULT_LAYOUT));
}

interface DashboardState {
  layout: Record<CardId, CardLayout>;
  /** When true, cards show drag + resize handles. */
  editing: boolean;
  toggleEditing: () => void;
  move: (id: CardId, x: number, y: number) => void;
  resize: (id: CardId, w: number, h: number) => void;
  toggleVisible: (id: CardId) => void;
  resetLayout: () => void;
}

export const useDashboardStore = create<DashboardState>()(
  persist(
    (set) => ({
      layout: cloneDefault(),
      editing: false,
      toggleEditing: () => set((s) => ({ editing: !s.editing })),
      move: (id, x, y) =>
        set((s) => ({ layout: { ...s.layout, [id]: { ...s.layout[id], x, y } } })),
      resize: (id, w, h) =>
        set((s) => ({ layout: { ...s.layout, [id]: { ...s.layout[id], w, h } } })),
      toggleVisible: (id) =>
        set((s) => ({
          layout: { ...s.layout, [id]: { ...s.layout[id], visible: !s.layout[id].visible } },
        })),
      resetLayout: () => set({ layout: cloneDefault() }),
    }),
    {
      name: "tagdash-dashboard",
      version: 1,
      // Merge persisted state over current defaults so cards added in a later
      // version still get a default slot, and never restore in edit mode.
      merge: (persisted, current) => {
        const p = (persisted ?? {}) as Partial<DashboardState>;
        return {
          ...current,
          ...p,
          layout: { ...cloneDefault(), ...(p.layout ?? {}) },
          editing: false,
        };
      },
    }
  )
);
