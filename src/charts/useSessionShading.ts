import { useEffect, type MutableRefObject } from "react";
import type { ISeriesApi } from "lightweight-charts";
import type { Bar, Timeframe } from "@/types";
import { toUTC } from "@/charts/chartOptions";
import { hexToRgba } from "@/charts/drawingsPrimitive";
import { isExtendedHours } from "@/lib/nyTime";
import type { ChartTheme } from "@/stores/chartThemeStore";

/** Full-height tint behind the candles on extended-hours bars (outside the
 *  09:30–16:00 NY cash session). Colour + opacity come from the theme. Intraday
 *  only — a daily bar spans a whole day. Paints on the dedicated `sessionBgRef`
 *  histogram (created in the chart-setup effect). */
export function useSessionShading(
  sessionBgRef: MutableRefObject<ISeriesApi<"Histogram"> | null>,
  bars: Bar[] | undefined,
  timeframe: Timeframe,
  theme: ChartTheme,
) {
  useEffect(() => {
    const series = sessionBgRef.current;
    if (!series) return;
    if (timeframe === "daily" || !bars?.length) {
      try { series.setData([]); } catch { /* */ }
      return;
    }
    const TINT  = hexToRgba(theme.session.color, theme.session.opacity); // extended hours
    const CLEAR = "rgba(0,0,0,0)";                                       // cash session
    const data = bars.map((b) => ({
      time:  toUTC(b.time),
      value: 1, // constant → fills full pane height on its own hidden scale
      color: isExtendedHours(b.time) ? TINT : CLEAR,
    }));
    try { series.setData(data); } catch { /* duplicate-time guard */ }
  }, [bars, timeframe, theme]);
}
