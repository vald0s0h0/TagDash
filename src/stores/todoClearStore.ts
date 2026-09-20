import { create } from "zustand";
import { persist } from "zustand/middleware";

// Trade ids the user cleared out of the sidebar's "À faire" list.
//
// That list is DERIVED from the DB (every trade still missing a screenshot or a
// journal), so "Vider" has nothing it could delete: it only records which trades
// to stop nagging about. The executions, the journal entries and the trade rows
// are untouched — TradesDbTab still shows them, and a cleared trade reappears
// nowhere. Persisted to localStorage so a clear survives a reload/restart.

interface TodoClearState {
  cleared: string[];
  /** Hide every trade currently pending. Takes the FULL list (not the already
   *  filtered one) and replaces the stored set with it: ids that have since been
   *  completed drop out on their own, so the set can't grow unbounded. */
  clearAll: (tradeIds: string[]) => void;
}

export const useTodoClearStore = create<TodoClearState>()(
  persist(
    (set) => ({
      cleared: [],
      clearAll: (tradeIds) => set({ cleared: tradeIds }),
    }),
    { name: "tagdash-todo-cleared", version: 1 },
  ),
);
