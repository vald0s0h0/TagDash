import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "@/lib/tauri";
import type { Timeframe } from "@/types";

const SNAPSHOT_KEY = ["market-snapshot"];
const LATENCY_KEY  = ["latency-status"];

/** Polls every 300 ms while a feed (live or mock) is active, every 2 s when idle. */
export function useMarketSnapshot() {
  return useQuery({
    queryKey: SNAPSHOT_KEY,
    queryFn:  () => api.getMarketSnapshot(),
    refetchInterval: (query) => {
      const d = query.state.data;
      return d?.live_running || d?.mock_running ? 300 : 2_000;
    },
  });
}

/** Current websocket-to-UI latency, polled every 500 ms. */
export function useLatencyStatus() {
  return useQuery({
    queryKey:        LATENCY_KEY,
    queryFn:         () => api.getLatencyStatus(),
    refetchInterval: 500,
  });
}

/** Start / stop the mock market feed. */
export function useMockFeed() {
  const qc = useQueryClient();

  const invalidate = () => {
    qc.invalidateQueries({ queryKey: SNAPSHOT_KEY });
    qc.invalidateQueries({ queryKey: LATENCY_KEY });
  };

  const start = useMutation({
    mutationFn: () => api.startMockMarketFeed(),
    onSuccess:  invalidate,
  });

  const stop = useMutation({
    mutationFn: () => api.stopMockMarketFeed(),
    onSuccess:  invalidate,
  });

  return { start, stop };
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
