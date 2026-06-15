import { useEffect, type MutableRefObject } from "react";
import type { IChartApi, ISeriesApi, LineData } from "lightweight-charts";
import type { Bar, PaneIndicator } from "@/types";
import {
  indicatorId, computeEma, computeSma, computeBollinger, computeDailyVwap,
  INDICATOR_COLORS, BOLLINGER_K, BOLLINGER_FILL,
} from "@/charts/indicators";
import type { BollingerPrimitive, BollingerBand } from "@/charts/bollingerPrimitive";
import { toUTC } from "@/charts/chartOptions";

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
) {
  const indicatorsKey = indicators.map(indicatorId).join(",");
  useEffect(() => {
    const chart = chartRef.current;
    if (!chart) return;

    // Desired LINE-series ids. Bollinger is a fill primitive (not series);
    // previous-day levels are price lines (drawn elsewhere); volume has its own
    // effect — none of those are reconciled here.
    const desired = new Set<string>();
    for (const ind of indicators) {
      if (
        ind.kind === "bollinger_bands" ||
        ind.kind === "previous_close" ||
        ind.kind === "previous_high" ||
        ind.kind === "previous_low" ||
        ind.kind === "volume"
      ) {
        continue; // not a line series managed by this reconcile loop
      }
      desired.add(indicatorId(ind));
    }
    for (const [id, series] of indicatorSeriesMap.current) {
      if (!desired.has(id)) {
        chart.removeSeries(series);
        indicatorSeriesMap.current.delete(id);
      }
    }

    if (!bars?.length) {
      bollingerPrimRef.current?.setData([]);
      return;
    }

    const times  = bars.map((b) => toUTC(b.time));
    const closes = bars.map((b) => b.close);

    // Bollinger → translucent fill band(s) via the primitive (one per requested
    // bollinger indicator). No data ⇒ cleared above; here we (re)compute.
    const bands: BollingerBand[] = [];
    for (const ind of indicators) {
      if (ind.kind !== "bollinger_bands") continue;
      const { upper, lower } = computeBollinger(closes, ind.period ?? 20, BOLLINGER_K);
      bands.push({ times, upper, lower, fill: BOLLINGER_FILL });
    }
    bollingerPrimRef.current?.setData(bands);

    for (const ind of indicators) {
      // Bollinger (primitive, above), previous-day levels (price lines) and volume
      // (own effect) are not line series.
      if (
        ind.kind === "bollinger_bands" ||
        ind.kind === "previous_close" ||
        ind.kind === "previous_high" ||
        ind.kind === "previous_low" ||
        ind.kind === "volume"
      ) {
        continue;
      }

      // Line indicators: vwap / ema / sma
      const id = indicatorId(ind);
      let series = indicatorSeriesMap.current.get(id) as ISeriesApi<"Line"> | undefined;
      if (!series) {
        series = chart.addLineSeries({
          color:                  INDICATOR_COLORS[ind.kind as keyof typeof INDICATOR_COLORS] ?? "#888",
          lineWidth:              1,
          priceLineVisible:       false,
          lastValueVisible:       false,
          crosshairMarkerVisible: false,
        });
        indicatorSeriesMap.current.set(id, series);
      }

      let values: (number | null)[];
      if (ind.kind === "vwap")      values = computeDailyVwap(bars);
      else if (ind.kind === "ema")  values = computeEma(closes, ind.period ?? 9);
      else if (ind.kind === "sma")  values = computeSma(closes, ind.period ?? 20);
      else                           values = bars.map(() => null);

      const data: LineData[] = [];
      values.forEach((v, i) => { if (v != null) data.push({ time: times[i], value: v }); });
      try { series.setData(data); } catch { /* duplicate-time guard */ }
    }
  }, [bars, indicatorsKey]); // eslint-disable-line react-hooks/exhaustive-deps
}
