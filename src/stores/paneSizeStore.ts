import { create } from "zustand";
import { persist } from "zustand/middleware";

// Per-layout pane sizing for chart zones. A zone's layout (columns of vertically
// stacked panes) is fixed by its strategy card, so sizes are keyed by a layout
// signature (`<strategy>|<panes-per-column>`) — every chart sharing that shape
// shares the user's split, and a card whose shape changes simply gets fresh
// defaults. Values are flex-grow ratios (all 1 by default); dragging an internal
// gutter redistributes grow between the two adjacent items only, so the rest of the
// layout is untouched. Persisted to localStorage. Outer chart edges are never
// resized here — only the gutters between panes / columns.

interface LayoutSizes {
  /** flex-grow per column (left → right). */
  cols: number[];
  /** flex-grow per pane, indexed [columnIndex][paneIndex]. */
  rows: number[][];
}

interface PaneSizeState {
  byLayout: Record<string, LayoutSizes>;
  setCols: (key: string, cols: number[]) => void;
  setRows: (key: string, colIdx: number, rows: number[]) => void;
}

export const usePaneSizeStore = create<PaneSizeState>()(
  persist(
    (set) => ({
      byLayout: {},
      setCols: (key, cols) =>
        set((s) => ({
          byLayout: {
            ...s.byLayout,
            [key]: { cols: [...cols], rows: s.byLayout[key]?.rows ?? [] },
          },
        })),
      setRows: (key, colIdx, rows) =>
        set((s) => {
          const prev = s.byLayout[key] ?? { cols: [], rows: [] };
          const nextRows = prev.rows.slice();
          nextRows[colIdx] = [...rows];
          return { byLayout: { ...s.byLayout, [key]: { cols: prev.cols, rows: nextRows } } };
        }),
    }),
    { name: "tagdash-pane-sizes" },
  ),
);
