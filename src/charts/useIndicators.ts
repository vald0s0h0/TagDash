import { useEffect, type MutableRefObject } from "react";
import type { IChartApi, ISeriesApi } from "lightweight-charts";
import type { Bar, PaneIndicator } from "@/types";
import { indicatorId } from "@/charts/indicators";
import type { BollingerPrimitive } from "@/charts/bollingerPrimitive";
import { applyLineIndicators } from "@/charts/chartDefaults";
import type { ChartTheme } from "@/stores/chartThemeStore";

/** Strategy-card-driven indicator series (VWAP / EMA / SMA / Bollinger), drawn on
 *  this pane. Reconciles the requested series against what's currently drawn, then
 *  (re)computes values from the loaded bars. VWAP is a session-anchored DAILY VWAP
 *  (computeDailyVwap), not Alpaca's per-bar `vw`. Bollinger is NOT line series — it
 *  is a single translucent fill rendered by `bollingerPrimRef` (the upper/basis/
 *  lower lines are intentionally gone). Previous-day levels and volume are handled
 *  by their own hooks. The line series live in `indicatorSeriesMap` (owned by the
 *  component, cleared by the chart teardown). */
export function useIndicators(
  chartRef: MutableRefObject<IChartApi | null>,
  indicatorSeriesMap: MutableRefObject<Map<string, ISeriesApi<"Line"> | ISeriesApi<"Histogram">>>,
  bollingerPrimRef: MutableRefObject<BollingerPrimitive | null>,
  bars: Bar[] | undefined,
  indicators: PaneIndicator[],
  theme: ChartTheme,
) {
  const indicatorsKey = indicators.map(indicatorId).join(",");
  useEffect(() => {
    const chart = chartRef.current;
    if (!chart) return;
    applyLineIndicators(chart, indicatorSeriesMap.current, bollingerPrimRef.current, bars, indicators, theme);
  }, [bars, indicatorsKey, theme]); // eslint-disable-line react-hooks/exhaustive-deps
}
