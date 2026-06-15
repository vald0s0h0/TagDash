import { useEffect, type MutableRefObject } from "react";
import type { IChartApi, ISeriesApi } from "lightweight-charts";
import type { CrosshairSync } from "@/lib/crosshairSync";

/** Register this pane in its zone's crosshair-sync group, so hovering it mirrors
 *  the crosshair onto the sibling same-symbol panes (and vice-versa). Re-registers
 *  on symbol change; apply/clear read the chart refs lazily at broadcast time. The
 *  broadcast side (subscribeCrosshairMove) lives in the chart-setup effect.
 *  Extracted verbatim from LightweightChart; same `[crosshairSync, paneId, symbol]`
 *  dependency, same behaviour (incl. returning the unregister cleanup). */
export function useCrosshairRegister(
  chartRef: MutableRefObject<IChartApi | null>,
  candleRef: MutableRefObject<ISeriesApi<"Candlestick"> | null>,
  crosshairSync: CrosshairSync | undefined,
  paneId: string | undefined,
  symbol: string,
) {
  useEffect(() => {
    if (!crosshairSync || !paneId) return;
    return crosshairSync.register(paneId, {
      symbol,
      apply: (time, price) => {
        const chart = chartRef.current;
        const candle = candleRef.current;
        if (!chart || !candle) return;
        try { chart.setCrosshairPosition(price, time, candle); } catch { /* out-of-range */ }
      },
      clear: () => {
        try { chartRef.current?.clearCrosshairPosition(); } catch { /* ignore */ }
      },
    });
  }, [crosshairSync, paneId, symbol]);
}
