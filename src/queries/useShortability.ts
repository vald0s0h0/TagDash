import { useQuery } from "@tanstack/react-query";
import { api } from "@/lib/tauri";
import type { Shortability } from "@/types";

/** Live Alpaca borrow classification, shared by chart headers and screener cards.
 * Only meaningful when Alpaca is the active broker: the internal TagDash book is a
 * simulated fill engine that does not enforce Alpaca's borrow restrictions, so an
 * NS/ETB/HTB badge would misrepresent what the active broker actually allows.
 * React Query deduplicates identical symbols, so opening a chart for a ticker that
 * is already in Pre-open does not spend a second Assets request; the broker-selection
 * query key matches ChartZone's, so the two share one poll instead of doubling it. */
export function useShortability(symbol: string | null | undefined): Shortability | null {
  const normalized = symbol?.trim().toUpperCase() ?? "";
  const { data: brokerSelection } = useQuery({
    queryKey: ["broker_selection"],
    queryFn: () => api.getBrokerSelection(),
    refetchInterval: 2000,
  });
  const alpacaSelected = brokerSelection?.broker === "alpaca";
  const query = useQuery({
    queryKey: ["alpaca-shortability", normalized],
    queryFn: () => api.getShortability(normalized),
    enabled: alpacaSelected && normalized.length > 0,
    staleTime: 60_000,
    refetchInterval: 60_000,
    retry: 1,
  });
  return alpacaSelected ? (query.data ?? null) : null;
}
