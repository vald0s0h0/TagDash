import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "@/lib/tauri";
import type { StrategyRiskConfig } from "@/types";

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
