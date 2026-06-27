import { useEffect, useRef, type MutableRefObject } from "react";
import type { ISeriesApi, IPriceLine } from "lightweight-charts";
import { limitOpts } from "@/charts/chartOptions";
import type { Side } from "@/types";

export interface LimitLineEntry {
  orderId: string;
  price:   number;
  side:    Side;
}

export function useLimitLines(
  candleRef: MutableRefObject<ISeriesApi<"Candlestick"> | null>,
  limitOrders: LimitLineEntry[],
) {
  const linesRef = useRef<Map<string, IPriceLine>>(new Map());

  useEffect(() => {
    const series = candleRef.current;
    if (!series) return;

    const current = linesRef.current;
    const incoming = new Set(limitOrders.map((o) => o.orderId));

    // Remove lines whose orders disappeared
    for (const [id, line] of current) {
      if (!incoming.has(id)) {
        series.removePriceLine(line);
        current.delete(id);
      }
    }

    // Create or update lines
    for (const o of limitOrders) {
      const existing = current.get(o.orderId);
      if (existing) {
        existing.applyOptions({ price: o.price });
      } else {
        const line = series.createPriceLine({ price: o.price, ...limitOpts(o.side) });
        current.set(o.orderId, line);
      }
    }
  }, [limitOrders]); // eslint-disable-line react-hooks/exhaustive-deps

  // Cleanup on unmount
  useEffect(() => {
    return () => {
      const series = candleRef.current;
      if (!series) return;
      for (const line of linesRef.current.values()) {
        try { series.removePriceLine(line); } catch { /* series already disposed */ }
      }
      linesRef.current.clear();
    };
  }, []); // eslint-disable-line react-hooks/exhaustive-deps
}
