import { useEffect, type MutableRefObject } from "react";
import type { ISeriesApi } from "lightweight-charts";
import type { Bar, Timeframe } from "@/types";
import { toUTC } from "@/charts/chartOptions";
import { isExtendedHours } from "@/lib/nyTime";

/** Barely-visible full-height tint behind the candles on extended-hours bars
 *  (outside the 09:30–16:00 NY cash session). Intraday only — a daily bar spans a
 *  whole day. Paints on the dedicated `sessionBgRef` histogram (created in the
 *  chart-setup effect). Extracted verbatim from LightweightChart; same
 *  `[bars, timeframe]` dependency, same behaviour. */
export function useSessionShading(
  sessionBgRef: MutableRefObject<ISeriesApi<"Histogram"> | null>,
  bars: Bar[] | undefined,
  timeframe: Timeframe,
) {
  useEffect(() => {
    const series = sessionBgRef.current;
    if (!series) return;
    if (timeframe === "daily" || !bars?.length) {
      try { series.setData([]); } catch { /* */ }
      return;
    }
    const TINT  = "rgba(130,150,190,0.02)"; // barely-there blue-grey, extended hours
    const CLEAR = "rgba(0,0,0,0)";          // transparent during the cash session
    const data = bars.map((b) => ({
      time:  toUTC(b.time),
      value: 1, // constant → fills full pane height on its own hidden scale
      color: isExtendedHours(b.time) ? TINT : CLEAR,
    }));
    try { series.setData(data); } catch { /* duplicate-time guard */ }
  }, [bars, timeframe]);
}
