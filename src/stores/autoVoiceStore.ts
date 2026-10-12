import { useEffect } from "react";
import { create } from "zustand";
import { api } from "@/lib/tauri";
import { useSttStore } from "@/stores/sttStore";
import type { OpenReviewSession } from "@/types";

// Auto voice recorder (Settings ▸ Revue, or the overlay toggle on the chart mic).
//
// When ON, the mic starts by itself the moment a reviewed ticker appears — i.e. its
// review session opens (the backend opens it on the scanner alert). One mic: while
// it records a ticker, a second ticker appearing does not steal it. « Libérer » on
// the recorded ticker stops and sends the note; the recorder then restarts on the
// most recent ticker still on screen with an open session.
//
// The chart mic buttons (MicDictate, toolbar variant) register the ticker they
// show; the one whose ticker is `pending` starts itself and clears it.

interface AutoVoiceState {
  enabled: boolean;
  /** Ticker whose mic should start now. */
  pending: string | null;
  /** Open sessions, newest first (polled). */
  sessions: OpenReviewSession[];
  /** Tickers shown by a mounted chart mic → number of mounted mics. */
  shown: Record<string, number>;
  /** Sessions the recorder already started on once (never auto-restart twice). */
  started: Set<string>;
  hydrate: () => Promise<void>;
  setEnabled: (on: boolean) => Promise<void>;
  setSessions: (s: OpenReviewSession[]) => void;
  register: (symbol: string) => void;
  unregister: (symbol: string) => void;
  /** A mic took the pending start. */
  consumed: (symbol: string) => void;
  /** The recorded ticker was released: move on to the most recent one. */
  released: (symbol: string) => void;
}

const micFree = () => useSttStore.getState().owner === null;

export const useAutoVoiceStore = create<AutoVoiceState>((set, get) => {
  // Pick the newest open session shown on screen, not started yet, not `except`.
  const candidate = (except?: string, allowStarted = false): string | null => {
    const { sessions, shown, started } = get();
    const s = sessions.find(
      (x) =>
        x.symbol !== except &&
        (shown[x.symbol] ?? 0) > 0 &&
        (allowStarted || !started.has(x.session_id)),
    );
    return s?.symbol ?? null;
  };
  const maybeStart = (except?: string, allowStarted = false) => {
    const st = get();
    if (!st.enabled || st.pending || !micFree()) return;
    const sym = candidate(except, allowStarted);
    if (sym) set({ pending: sym });
  };

  return {
    enabled: false,
    pending: null,
    sessions: [],
    shown: {},
    started: new Set(),
    hydrate: async () => {
      try {
        const cfg = await api.getReviewConfig();
        set({ enabled: cfg.auto_voice });
      } catch {
        /* backend not ready */
      }
    },
    setEnabled: async (on) => {
      set({ enabled: on, pending: on ? get().pending : null });
      try {
        const cfg = await api.getReviewConfig();
        await api.setReviewConfig({ ...cfg, auto_voice: on });
      } catch {
        /* keep the local switch; retried on the next toggle */
      }
      if (on) maybeStart();
    },
    setSessions: (sessions) => {
      set({ sessions });
      maybeStart();
    },
    register: (symbol) => {
      set((st) => ({ shown: { ...st.shown, [symbol]: (st.shown[symbol] ?? 0) + 1 } }));
      maybeStart();
    },
    unregister: (symbol) => {
      set((st) => {
        const n = (st.shown[symbol] ?? 1) - 1;
        const shown = { ...st.shown };
        if (n > 0) shown[symbol] = n;
        else delete shown[symbol];
        return { shown, pending: st.pending === symbol ? null : st.pending };
      });
    },
    consumed: (symbol) => {
      const st = get();
      const s = st.sessions.find((x) => x.symbol === symbol);
      const started = new Set(st.started);
      if (s) started.add(s.session_id);
      set({ pending: null, started });
    },
    released: (symbol) => {
      // The mic is released a tick later (MicDictate.finish) — defer the pick.
      setTimeout(() => maybeStart(symbol, true), 50);
    },
  };
});

/** Mounted once (App): loads the switch and polls the open review sessions. */
export function useAutoVoiceSync() {
  useEffect(() => {
    useAutoVoiceStore.getState().hydrate();
    let alive = true;
    const tick = () =>
      api
        .reviewOpenSessions()
        .then((s) => alive && useAutoVoiceStore.getState().setSessions(s))
        .catch(() => {});
    tick();
    const t = setInterval(tick, 1500);
    return () => {
      alive = false;
      clearInterval(t);
    };
  }, []);
}
