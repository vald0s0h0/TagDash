import { useEffect, type MutableRefObject } from "react";
import type { IChartApi, ISeriesApi } from "lightweight-charts";
import type { Bar } from "@/types";
import { toUTC } from "@/charts/chartOptions";
import { hexToRgba } from "@/charts/drawingsPrimitive";
import type { ChartTheme } from "@/stores/chartThemeStore";

/** Bottom-pinned volume histogram, drawn on every pane from the loaded bars (no
 *  strategy-card opt-in). The series is created lazily on first data and lives in
 *  `volumeSeriesRef`, which the chart's own teardown nulls — so the ref is owned
 *  by the component and passed in here. Up/down bars reuse the candle colours at
 *  the theme's volume opacity, so a candle-colour edit re-tints the volume too. */
export function useVolumeSeries(
  chartRef: MutableRefObject<IChartApi | null>,
  volumeSeriesRef: MutableRefObject<ISeriesApi<"Histogram"> | null>,
  bars: Bar[] | undefined,
  theme: ChartTheme,
) {
  useEffect(() => {
    const chart = chartRef.current;
    if (!chart) return;
    if (!bars?.length) return;
    if (!volumeSeriesRef.current) {
      volumeSeriesRef.current = chart.addHistogramSeries({
        priceFormat:      { type: "volume" },
        priceScaleId:     "", // overlay scale, pinned to the bottom below
        priceLineVisible: false,
        lastValueVisible: false,
      });
      volumeSeriesRef.current.priceScale().applyOptions({ scaleMargins: { top: 0.82, bottom: 0 } });
    }
    const up   = hexToRgba(theme.candle.up, theme.volume.upOpacity);
    const down = hexToRgba(theme.candle.down, theme.volume.downOpacity);
    const data = bars.map((b) => ({
      time:  toUTC(b.time),
      value: b.volume,
      color: b.close >= b.open ? up : down,
    }));
    try { volumeSeriesRef.current.setData(data); } catch { /* duplicate-time guard */ }
  }, [bars, theme]); // eslint-disable-line react-hooks/exhaustive-deps
}
