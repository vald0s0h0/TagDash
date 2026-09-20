import { useQuery } from "@tanstack/react-query";
import { api } from "@/lib/tauri";
import type { TickerLiveState, Timeframe } from "@/types";

const SNAPSHOT_KEY = ["market-snapshot"];
const LATENCY_KEY  = ["latency-status"];

/** Polls every 300 ms while the live feed is active, every 2 s when idle. */
export function useMarketSnapshot() {
  return useQuery({
    queryKey: SNAPSHOT_KEY,
    queryFn:  () => api.getMarketSnapshot(),
    refetchInterval: (query) => {
      const d = query.state.data;
      return d?.live_running ? 300 : 2_000;
    },
  });
}

/** Live state of one symbol, out of the shared snapshot. Every consumer that shows
 *  a live price / volume / change reads THIS — sidebar cards and the band above the
 *  chart alike — so the same ticker can never print two different numbers in two
 *  places (which is worse than a stale number: it makes the user doubt both). One
 *  React-Query cache entry, one poll, one set of values. */
export function useLiveTicker(symbol: string | null | undefined): TickerLiveState | null {
  const { data } = useMarketSnapshot();
  return symbol ? data?.tickers[symbol] ?? null : null;
}

/** Current websocket-to-UI latency, polled every 500 ms. */
export function useLatencyStatus() {
  return useQuery({
    queryKey:        LATENCY_KEY,
    queryFn:         () => api.getLatencyStatus(),
    refetchInterval: 500,
  });
}

/** Closed candles for one symbol / timeframe. Refreshes every second. */
export function useTickerBars(symbol: string, timeframe: Timeframe) {
  return useQuery({
    queryKey:        ["ticker-bars", symbol, timeframe],
    queryFn:         () => api.getTickerBars(symbol, timeframe),
    enabled:         !!symbol,
    staleTime:       1_000,
    refetchInterval: 1_000,
  });
}
