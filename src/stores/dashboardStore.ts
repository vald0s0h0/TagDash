import { create } from "zustand";
import { persist } from "zustand/middleware";

// The dashboard grid is a fixed, invisible 20×12 board. Cards are placed and sized
// in whole grid cells (0-indexed x/y, 1-based span via CSS grid). Layout +
// visibility + per-card surface persist to localStorage so the user's arrangement
// survives restarts.

export const GRID_COLS = 20;
export const GRID_ROWS = 12;

/** Frosted/Brutal surface variant, chosen per card in edit mode. */
export type Surface = "glass" | "heavy" | "invisible";

/** Built-in card ids. Add new cards here + in `components/dashboard/cards.tsx`
 *  and give them a default slot in DEFAULT_LAYOUT. */
export type CardId =
  | "kpis"
  | "pnl-curve"
  | "rolling-pf"
  | "journal"
  | "inspiration"
  | "quote"
  | "heading";

export interface CardLayout {
  x: number;
  y: number;
  w: number;
  h: number;
  visible: boolean;
  surface: Surface;
}

// Coordinates are in the 20×12 grid (the old 10×6 layout, ×2).
export const DEFAULT_LAYOUT: Record<CardId, CardLayout> = {
  "kpis":        { x: 0,  y: 0, w: 8,  h: 4, visible: true, surface: "glass" },
  "inspiration": { x: 0,  y: 4, w: 8,  h: 4, visible: true, surface: "glass" },
  "quote":       { x: 0,  y: 8, w: 8,  h: 4, visible: true, surface: "glass" },
  "pnl-curve":   { x: 8,  y: 0, w: 12, h: 6, visible: true, surface: "glass" },
  "rolling-pf":  { x: 8,  y: 6, w: 6,  h: 6, visible: true, surface: "glass" },
  "journal":     { x: 14, y: 6, w: 6,  h: 6, visible: true, surface: "glass" },
  // Extra card — off by default (the default board is already full). Enable it in
  // the Cartes menu and drag it into place.
  "heading":     { x: 0,  y: 0, w: 8,  h: 2, visible: false, surface: "glass" },
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
  setSurface: (id: CardId, surface: Surface) => void;
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
      setSurface: (id, surface) =>
        set((s) => ({ layout: { ...s.layout, [id]: { ...s.layout[id], surface } } })),
      resetLayout: () => set({ layout: cloneDefault() }),
    }),
    {
      name: "tagdash-dashboard",
      // v2: grid doubled to 20×12 and per-card `surface` added.
      version: 2,
      // Old v1 layouts were authored on the 10×6 grid with no surface — scale
      // coordinates ×2 and default every card to glass.
      migrate: (persisted, version) => {
        const p = (persisted ?? {}) as Partial<DashboardState>;
        if (version < 2 && p.layout) {
          const scaled: Record<string, CardLayout> = {};
          for (const [id, l] of Object.entries(p.layout)) {
            const old = l as Partial<CardLayout>;
            scaled[id] = {
              x: (old.x ?? 0) * 2,
              y: (old.y ?? 0) * 2,
              w: (old.w ?? 1) * 2,
              h: (old.h ?? 1) * 2,
              visible: old.visible ?? true,
              surface: old.surface ?? "glass",
            };
          }
          p.layout = scaled as Record<CardId, CardLayout>;
        }
        return p as DashboardState;
      },
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
