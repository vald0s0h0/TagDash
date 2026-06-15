import { useEffect, useRef, type MutableRefObject } from "react";
import type { ISeriesApi, CandlestickData } from "lightweight-charts";
import type { Bar } from "@/types";
import { toUTC } from "@/charts/chartOptions";

/** Feed the candle series following the official lightweight-charts protocol:
 *  `setData` for the initial load, older-history prepends, AND any change in the
 *  MIDDLE of the series (a gap getting back-filled by a later history refresh —
 *  setData preserves the visible TIME range, so the view is kept); `update()` only
 *  for routine tail growth / the last candle mutating (never touches the view).
 *  The render bookkeeping lives in renderedFirst/LastRef + lastBarRef (owned by the
 *  component), plus a local rendered-count ref to spot middle inserts/removals. */
export function useBarSeries(
  candleRef: MutableRefObject<ISeriesApi<"Candlestick"> | null>,
  bars: Bar[] | undefined,
  renderedFirstRef: MutableRefObject<number | null>,
  renderedLastRef: MutableRefObject<number | null>,
  lastBarRef: MutableRefObject<CandlestickData | null>,
) {
  // How many bars are currently in the series — lets us tell a pure tail append
  // (count grew by exactly the number of new tail bars) from a middle gap-fill
  // (count grew/shrank without the tail changing accordingly), which `update()`
  // alone would silently drop.
  const renderedCountRef = useRef<number>(0);
  useEffect(() => {
    const series = candleRef.current;
    if (!series || !bars?.length) return;

    const data: CandlestickData[] = bars.map((b) => ({
      time:  toUTC(b.time),
      open:  b.open,
      high:  b.high,
      low:   b.low,
      close: b.close,
    }));
    const firstSec = data[0].time as number;
    const lastSec  = data[data.length - 1].time as number;
    const prevFirst = renderedFirstRef.current;
    const prevLast  = renderedLastRef.current;
    const prevCount = renderedCountRef.current;

    // Bars newer than the previously-rendered tail = the only ones `update()` may
    // legally push (it requires times ≥ the series' last bar).
    const tailAdded =
      prevLast == null ? data.length : data.filter((d) => (d.time as number) > prevLast).length;
    // A pure tail change keeps the front, never moves the last bar backwards, and
    // grows by EXACTLY the tail bars (the last bar may also mutate in place). Any
    // other delta means a bar was inserted/removed in the middle (a back-filled
    // gap) → those edits are invisible to `update()`, so a full `setData` is the
    // only way to draw them.
    const isPureTail =
      prevFirst != null &&
      prevLast  != null &&
      firstSec === prevFirst &&
      lastSec  >= prevLast &&
      data.length === prevCount + tailAdded;

    if (!isPureTail) {
      try { series.setData(data); } catch { /* duplicate-time guard */ }
    } else {
      // Tail-only change: update the previously-last bar (it may have just closed)
      // and any bars appended since, in ascending order.
      let start = data.length - 1;
      while (start > 0 && (data[start - 1].time as number) >= prevLast) start--;
      for (let i = start; i < data.length; i++) {
        try { series.update(data[i]); } catch { /* out-of-order guard */ }
      }
    }

    renderedFirstRef.current = firstSec;
    renderedLastRef.current  = lastSec;
    renderedCountRef.current = data.length;
    lastBarRef.current = data[data.length - 1];
  }, [bars]);
}
