import { useEffect, type MutableRefObject } from "react";
import { useQuery } from "@tanstack/react-query";
import type { Bar } from "@/types";
import { api } from "@/lib/tauri";
import type { ExecutionsPrimitive } from "@/charts/executionsPrimitive";
import { toUTC } from "@/charts/chartOptions";

/** Trade-execution markers (entry/scale/exit triangles + the connecting P&L line)
 *  for a symbol — persisted per ticker (multi-day) and polled so new fills appear
 *  live. Pushed into the chart's `ExecutionsPrimitive` whenever the executions or
 *  the bar set change (fills are snapped to bar times). Extracted verbatim from
 *  LightweightChart; same `[executions, bars]` dependency, same behaviour. */
export function useExecutionMarkers(
  execPrimRef: MutableRefObject<ExecutionsPrimitive | null>,
  symbol: string,
  bars: Bar[] | undefined,
) {
  const { data: executions } = useQuery({
    queryKey: ["executions", symbol],
    queryFn:  () => api.getExecutionsForSymbol(symbol),
    enabled:  !!symbol,
    refetchInterval: 2000,
  });
  useEffect(() => {
    const times = bars?.map((b) => toUTC(b.time) as number) ?? [];
    execPrimRef.current?.setData(executions ?? [], times);
  }, [executions, bars]);
}
