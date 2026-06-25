import { create } from "zustand";
import type { Session } from "@/types";
import { api } from "@/lib/tauri";

type Modal = "settings" | "sync-status" | "startup" | "feed-diagnostics" | "news-debug" | "bug-report" | "tickers-table" | "flat-files" | "stt" | "update" | null;

/** Top-level view: the trading workspace (sessions + charts + sidebar), the
 *  standalone KPI dashboard (moodboard, no sidebar), or the embedded TradeTally
 *  web app (no sidebar). */
export type View = "trading" | "dashboard" | "tradetally";

interface UiState {
  /** Which top-level view is shown. The dashboard (Moon, first in the rail) hides
   *  the sidebar; the session tabs switch back to the trading workspace. */
  activeView:        View;
  setActiveView:     (v: View) => void;
  activeSession:     Session;
  logsOpen:          boolean;
  openModal:         Modal;
  selectedTicker:    string | null;
  /** Symbols the user dismissed from the pre-open screener. Persisted per trading
   *  day (DB) so dismissals survive a restart and reset the next day. */
  dismissedScreener: string[];
  /** Market Replay toolbar visibility (toggled from the LeftRail menu; the
   *  toolbar only renders when this is on). */
  replayOpen:        boolean;
  toggleReplay:      () => void;
  setActiveSession:  (s: Session) => void;
  toggleLogs:        () => void;
  showModal:         (m: Modal) => void;
  closeModal:        () => void;
  setSelectedTicker: (symbol: string | null) => void;
  dismissScreener:   (symbol: string) => void;
  /** Replace the dismissed list (hydrated from the DB on launch). */
  setDismissedScreener: (symbols: string[]) => void;
}

export const useUiStore = create<UiState>((set) => ({
  // Default to the trading workspace so the startup pipeline / scanner flow is
  // unchanged; the Moon dashboard is opt-in (first button in the rail).
  activeView:        "trading",
  setActiveView:     (v) => set({ activeView: v }),
  // Start on the premarket tab (where the trading session begins) and surface the
  // startup pipeline modal so launch progress is visible right away.
  activeSession:     "premarket",
  logsOpen:          false,
  openModal:         "startup",
  selectedTicker:    null,
  dismissedScreener: [],
  replayOpen:        false,

  toggleReplay:      () => set((state) => ({ replayOpen: !state.replayOpen })),
  setActiveSession:  (s) => set({ activeSession: s }),
  toggleLogs:        () => set((state) => ({ logsOpen: !state.logsOpen })),
  showModal:         (m) => set({ openModal: m }),
  closeModal:        () => set({ openModal: null }),
  setSelectedTicker: (symbol) => set({ selectedTicker: symbol }),
  dismissScreener:   (symbol) => {
    // Persist for the day (fire-and-forget); update local state immediately.
    api.dismissScreener(symbol).catch(() => {});
    set((state) =>
      state.dismissedScreener.includes(symbol)
        ? {}
        : { dismissedScreener: [...state.dismissedScreener, symbol] }
    );
  },
  setDismissedScreener: (symbols) => set({ dismissedScreener: symbols }),
}));
