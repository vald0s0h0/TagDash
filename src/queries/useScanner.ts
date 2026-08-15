import { useMemo } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "@/lib/tauri";
import type { PanicLlmScore, StrategyRiskConfig } from "@/types";

export function useStrategies() {
  return useQuery({
    queryKey: ["strategies"],
    queryFn: api.getStrategies,
    staleTime: Infinity, // strategy metadata is static; enabled flag toggled below
  });
}

/** Toggle a strategy on/off at runtime (persisted backend-side). */
export function useSetStrategyEnabled() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ id, enabled }: { id: string; enabled: boolean }) =>
      api.setStrategyEnabled(id, enabled),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["strategies"] });
    },
  });
}

/** Set the full risk config for a strategy at runtime (persisted backend-side). */
export function useSetStrategyRisk() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ id, risk }: { id: string; risk: StrategyRiskConfig }) =>
      api.setStrategyRisk(id, risk),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["strategies"] });
    },
  });
}

/** Identity cards keyed by strategy id. Static — fetched once and cached. */
export function useStrategyCards() {
  return useQuery({
    queryKey: ["strategy-cards"],
    queryFn:  api.getStrategyCards,
    staleTime: Infinity,
  });
}

export function useActiveAlerts() {
  return useQuery({
    queryKey: ["active-alerts"],
    queryFn:  api.getActiveAlerts,
    refetchInterval: 800,
  });
}

export function useAlertHistory() {
  return useQuery({
    queryKey: ["alert-history"],
    queryFn:  api.getAlertHistory,
    refetchInterval: 2000,
  });
}

/** Live pre-open screener matches (tickers currently meeting strategy criteria). */
export function useScreenerMatches() {
  return useQuery({
    queryKey: ["screener-matches"],
    queryFn:  api.getScreenerMatches,
    refetchInterval: 800,
  });
}

/** Pinned screener symbols. Polled rather than held in a store because the backend
 *  serves a DIFFERENT set in live mode and during a Market Replay (a backtest must
 *  not pollute the real watchlist) — polling makes the switch automatic instead of
 *  needing a replay-transition listener. */
export function useScreenerPins() {
  return useQuery({
    queryKey: ["screener-pins"],
    queryFn:  api.getScreenerPins,
    refetchInterval: 2000,
  });
}

/** Toggle a pin, then refresh both the pin list and the screener rows (a pinned
 *  ticker that no longer scores is injected server-side, so the row list changes). */
export function useTogglePin() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ symbol, pinned }: { symbol: string; pinned: boolean }) =>
      pinned ? api.unpinScreener(symbol) : api.pinScreener(symbol),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["screener-pins"] });
      qc.invalidateQueries({ queryKey: ["screener-matches"] });
    },
  });
}

/** The user's own 1..3 read of each ticker's potential. Purely a memo — nothing on
 *  the platform consumes it — so it is fetched on the same cheap poll as the pins. */
export function useScreenerRatings() {
  return useQuery({
    queryKey: ["screener-ratings"],
    queryFn:  api.getScreenerRatings,
    refetchInterval: 2000,
  });
}

/** Set (or clear, with 0) a rating. Only the rating list is invalidated: unlike a
 *  pin, a rating never changes which rows the server returns. */
export function useSetRating() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ symbol, rating }: { symbol: string; rating: number }) =>
      api.setScreenerRating(symbol, rating),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["screener-ratings"] });
    },
  });
}

// ─── Panic Score (LLM watchlist ranking) ──────────────────────────────────────

/** Progress of the current (or last) Panic Score run.
 *
 *  Polled fast while a run is in flight so the button's counter advances ticker by
 *  ticker, and slowly otherwise — a run only starts on a click, so there is nothing
 *  to watch for the rest of the session. */
export function usePanicRun() {
  return useQuery({
    queryKey: ["panic-llm-run"],
    queryFn:  api.getPanicLlmRun,
    refetchInterval: (q) => (q.state.data?.running ? 500 : 5000),
  });
}

/** The stored analysis for the current day + mode, keyed by symbol for the report
 *  overlay. Day- and mode-scoped server-side, so switching in or out of a Market
 *  Replay swaps the set automatically rather than needing a transition listener —
 *  the same reason the pins are polled. */
export function usePanicLlmScores() {
  const query = useQuery({
    queryKey: ["panic-llm-scores"],
    queryFn:  api.getPanicLlmScores,
    refetchInterval: 2000,
  });
  const bySymbol = useMemo(() => {
    const map: Record<string, PanicLlmScore> = {};
    for (const s of query.data ?? []) map[s.symbol] = s;
    return map;
  }, [query.data]);
  return { ...query, bySymbol };
}

/** Launch the analysis over an explicit symbol list (the visible watchlist).
 *
 *  Invalidates the run state immediately so the button flips to its pending look
 *  without waiting for the next poll tick. */
export function useRunPanicLlm() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (symbols: string[]) => api.runPanicLlmAnalysis(symbols),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["panic-llm-run"] });
    },
  });
}

/** Whether a Tavily/Brave key is configured — the button explains that without one
 *  the context cascade stops at the structured sources. Static for the session
 *  unless the user edits their keys, hence the long stale time. */
export function useHasWebSearch() {
  return useQuery({
    queryKey: ["has-web-search"],
    queryFn:  api.hasWebSearchProvider,
    staleTime: 60_000,
  });
}

export function useStartScanner() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: api.startScanner,
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["active-alerts"] });
    },
  });
}

export function useStopScanner() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: api.stopScanner,
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["active-alerts"] });
    },
  });
}
