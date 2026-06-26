import { useEffect, type MutableRefObject } from "react";
import type { ISeriesApi, SeriesMarker, Time, UTCTimestamp } from "lightweight-charts";
import type { Bar } from "@/types";
import { toUTC } from "@/charts/chartOptions";

// A marker whose time isn't an exact bar time is dropped by lightweight-charts'
// `setMarkers`, so a split's ex-date (UTC midnight) never lined up with the daily
// bar (Alpaca stamps daily bars a few hours into the UTC day) — the markers were
// computed correctly but silently discarded. Snapping each marker to its nearest
// loaded bar (within a day) fixes that regardless of the backend's date convention.
const SNAP_TOLERANCE_SEC = 36 * 60 * 60; // 1.5 days — covers any intraday offset

/** Nearest bar time (seconds) to `t`, or null if none within tolerance. */
function nearestBarTime(barTimes: number[], t: number): number | null {
  if (barTimes.length === 0) return null;
  let lo = 0, hi = barTimes.length - 1;
  if (t <= barTimes[0]) return barTimes[0] - t <= SNAP_TOLERANCE_SEC ? barTimes[0] : null;
  if (t >= barTimes[hi]) return t - barTimes[hi] <= SNAP_TOLERANCE_SEC ? barTimes[hi] : null;
  while (lo <= hi) {
    const mid = (lo + hi) >> 1;
    if (barTimes[mid] === t) return t;
    if (barTimes[mid] < t) lo = mid + 1; else hi = mid - 1;
  }
  // lo = first bar after t, hi = last bar before t.
  const before = barTimes[hi];
  const after  = barTimes[lo];
  const best   = t - before <= after - t ? before : after;
  return Math.abs(best - t) <= SNAP_TOLERANCE_SEC ? best : null;
}

/** Candle markers (e.g. red dots on split days) on the candle series, snapped to
 *  the nearest loaded bar so they actually render, and reconciled by a content key
 *  so `setMarkers` only runs when the marker set OR the loaded bar range changes. */
type ChartMarker = {
  time: number; color: string; text?: string;
  position?: "aboveBar" | "belowBar" | "inBar";
  shape?: "circle" | "square" | "arrowUp" | "arrowDown";
};

export function useCandleMarkers(
  candleRef: MutableRefObject<ISeriesApi<"Candlestick"> | null>,
  markers: ChartMarker[],
  bars: Bar[] | undefined,
) {
  const markersKey = markers.map((m) => `${m.time}:${m.color}:${m.position ?? ""}:${m.shape ?? ""}:${m.text ?? ""}`).join(",");
  // Re-snap when the loaded window grows/shifts (back-fill), keyed by first/last bar.
  const barsKey = bars?.length ? `${toUTC(bars[0].time)}-${toUTC(bars[bars.length - 1].time)}-${bars.length}` : "";
  useEffect(() => {
    const series = candleRef.current;
    if (!series) return;
    const barTimes = bars?.length ? bars.map((b) => toUTC(b.time) as number) : [];
    const data: SeriesMarker<Time>[] = [];
    for (const m of markers) {
      const snapped = nearestBarTime(barTimes, m.time);
      if (snapped == null) continue; // off the loaded range → appears once back-filled
      data.push({
        time:     snapped as UTCTimestamp,
        position: m.position ?? "belowBar",
        color:    m.color,
        shape:    m.shape ?? "circle",
        text:     m.text,
      });
    }
    // setMarkers wants ascending, de-duplicated times.
    data.sort((a, b) => (a.time as number) - (b.time as number));
    try { series.setMarkers(data); } catch { /* ignore out-of-range marker */ }
  }, [markersKey, barsKey]); // eslint-disable-line react-hooks/exhaustive-deps
}
