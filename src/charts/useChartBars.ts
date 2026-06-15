import { useState, useEffect, useRef, useCallback, type MutableRefObject } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import type { IChartApi } from "lightweight-charts";
import type { Bar, Timeframe } from "@/types";
import { api } from "@/lib/tauri";
import { toUTC, BACKFILL_THRESHOLD, BACKFILL_BATCH } from "@/charts/chartOptions";

/** Bar-loading pipeline for a (symbol, timeframe): a slow "history refresh" query
 *  (backend refetches from Alpaca — fills gaps + today's forming bar) nudges a fast
 *  RAM poll; both are merged into an accumulator (live wins per time slot) that also
 *  takes lazily back-filled older history, producing one continuous ascending
 *  series. Returns the rendered `bars` + `loadOlderBars` (called by the chart's
 *  visible-range handler). Shared refs are passed in: `chartRef` (read the visible
 *  range), `symbol/timeframeRef` (current values for in-flight guards), `barsRef`
 *  (kept in sync for the click R-ratio), and `renderedFirst/LastRef` (reset on
 *  series change so the next render is a full setData). Extracted verbatim from
 *  LightweightChart. */
export function useChartBars(
  symbol: string,
  timeframe: Timeframe,
  chartRef: MutableRefObject<IChartApi | null>,
  symbolRef: MutableRefObject<string>,
  timeframeRef: MutableRefObject<Timeframe>,
  barsRef: MutableRefObject<Bar[] | undefined>,
  renderedFirstRef: MutableRefObject<number | null>,
  renderedLastRef: MutableRefObject<number | null>,
): { bars: Bar[] | undefined; loadOlderBars: () => Promise<void> } {
  const queryClient = useQueryClient();
  // Accumulated bar history (keyed by Unix-seconds): the live poll upserts recent
  // bars (live wins) while scroll-back lazily prepends older batches.
  const accumRef        = useRef<Map<number, Bar>>(new Map());
  const loadingOlderRef = useRef<boolean>(false);
  const noMoreOlderRef  = useRef<boolean>(false);
  const [bars, setBars] = useState<Bar[] | undefined>(undefined);

  // ── Unified bar load (refresh on open): the backend refreshes history from
  // Alpaca (gaps + today's forming bar) and merges into RAM; the fast RAM poll
  // below renders it. Sub-minute frames Alpaca can't serve → RAM only, no interval.
  const { data: backfilled } = useQuery({
    queryKey: ["chart_history", symbol, timeframe],
    queryFn:  () => api.loadChartBars(symbol, timeframe),
    enabled:  !!symbol,
    refetchOnMount: "always",
    refetchOnWindowFocus: false,
    refetchInterval:
      timeframe === "5s" || timeframe === "10s" ? false
      : timeframe === "daily" ? 30_000
      : 15_000,
    retry: false,
  });
  // Once a refresh lands, nudge the live bars query so it renders immediately.
  useEffect(() => {
    if (backfilled && backfilled.length > 0) {
      queryClient.invalidateQueries({ queryKey: ["bars", symbol, timeframe] });
    }
  }, [backfilled, symbol, timeframe, queryClient]);

  // ── Poll live bars from RAM (cheap, fast).
  const { data: fetchedBars } = useQuery({
    queryKey: ["bars", symbol, timeframe],
    queryFn:  () => api.getTickerBars(symbol, timeframe),
    refetchInterval: timeframe === "5s" || timeframe === "10s" ? 500 : 1000,
    enabled:  !!symbol,
  });

  useEffect(() => { barsRef.current = bars; }, [bars]); // eslint-disable-line react-hooks/exhaustive-deps

  // Reset the accumulator when the symbol or timeframe changes (different series).
  useEffect(() => {
    accumRef.current = new Map();
    loadingOlderRef.current = false;
    noMoreOlderRef.current  = false;
    // Force a full setData (not an update) for the first render of the new series.
    renderedFirstRef.current = null;
    renderedLastRef.current  = null;
    setBars(undefined);
  }, [symbol, timeframe]); // eslint-disable-line react-hooks/exhaustive-deps

  // Merge each live poll into the accumulator (live bars win for their slot) and
  // re-render the full ascending series.
  useEffect(() => {
    if (!fetchedBars?.length) return;
    const accum = accumRef.current;
    for (const b of fetchedBars) accum.set(toUTC(b.time) as number, b);
    setBars([...accum.entries()].sort((a, c) => a[0] - c[0]).map((e) => e[1]));
  }, [fetchedBars]);

  // ── Lazy back-fill of older history (scroll/zoom into the past). When the
  // visible range nears the left edge, fetch a batch of older bars from Alpaca
  // (ending before the oldest loaded bar) and prepend them. Guarded against
  // concurrent / dead-end fetches.
  const loadOlderBars = useCallback(async () => {
    const sym = symbolRef.current;
    const tf  = timeframeRef.current;
    if (loadingOlderRef.current || noMoreOlderRef.current) return;
    if (tf === "5s" || tf === "10s") return; // Alpaca REST can't serve sub-minute
    if (accumRef.current.size === 0) return;

    loadingOlderRef.current = true;
    try {
      // Read the visible range once and estimate the new left-edge index from the
      // bars we prepend (the chart can't re-render mid-loop; data lands via the
      // render effect asynchronously). After setData, lightweight-charts preserves
      // the visible TIME range, so the logical `from` jumps up by the prepended
      // count and the next range-change event only re-fires if the user keeps
      // scrolling — self-correcting.
      const startRange = chartRef.current?.timeScale().getVisibleLogicalRange();
      const from0  = startRange ? startRange.from : 0;
      const target = BACKFILL_THRESHOLD + BACKFILL_BATCH;
      const accum  = accumRef.current;
      let totalAdded = 0;

      for (let i = 0; i < 10; i++) {
        // Oldest currently-loaded bar = the back-fill cutoff.
        let oldestSec = Infinity;
        let oldestIso = "";
        for (const [sec, b] of accum) {
          if (sec < oldestSec) { oldestSec = sec; oldestIso = b.time; }
        }
        if (!oldestIso) break;

        const older = await api.loadOlderBars(sym, tf, oldestIso, BACKFILL_BATCH);
        // The chart may have switched symbol/timeframe while the request was in
        // flight — discard the stale batch rather than poison the new accumulator.
        if (symbolRef.current !== sym || timeframeRef.current !== tf) return;
        if (!older?.length) { noMoreOlderRef.current = true; break; }

        let added = 0;
        for (const b of older) {
          const sec = toUTC(b.time) as number;
          if (!accum.has(sec)) { accum.set(sec, b); added++; }
        }
        if (added === 0) { noMoreOlderRef.current = true; break; } // reached data start
        totalAdded += added;
        if (from0 + totalAdded > target) break;
      }

      // One render after the whole batch: the render effect sees a changed FIRST
      // timestamp → full setData (which keeps the user's view) instead of update().
      if (totalAdded > 0) {
        setBars([...accum.entries()].sort((a, c) => a[0] - c[0]).map((e) => e[1]));
      }
    } catch { /* soft-fail */ }
    finally { loadingOlderRef.current = false; }
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  return { bars, loadOlderBars };
}
