import { useState, useEffect, useRef, useCallback, type MutableRefObject } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import type { Bar, Timeframe } from "@/types";
import { api } from "@/lib/tauri";
import { toUTC, BACKFILL_BATCH } from "@/charts/chartOptions";

/** Bar-loading pipeline for a (symbol, timeframe): a slow "history refresh" query
 *  (backend refetches from Alpaca — fills gaps + today's forming bar) nudges a fast
 *  RAM poll; both are merged into an accumulator (live wins per time slot) that also
 *  takes lazily back-filled older history, producing one continuous ascending
 *  series. Returns the rendered `bars` + `loadOlderBars` (called by the chart's
 *  visible-range handler). Shared refs are passed in: `symbol/timeframeRef` (current
 *  values for in-flight guards), `barsRef` (kept in sync for the click R-ratio), and
 *  `renderedFirst/LastRef` (reset on series change so the next render is a full
 *  setData). Extracted verbatim from LightweightChart. */
export function useChartBars(
  symbol: string,
  timeframe: Timeframe,
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
  // `gcTime: 0` drops this query's cache the moment the symbol/timeframe changes
  // (the observer goes inactive). Without it, returning to a previously-viewed
  // ticker would synchronously deliver that visit's STALE snapshot, which the
  // accumulator merges with the now-current RAM ring — and if enough time elapsed
  // that the two no longer overlap (the 5s/10s ring only spans the last few
  // minutes), the union is non-contiguous → the intermittent "holes" on switch.
  // Starting each visit from a fresh RAM fetch keeps the series contiguous.
  const { data: fetchedBars } = useQuery({
    queryKey: ["bars", symbol, timeframe],
    queryFn:  () => api.getTickerBars(symbol, timeframe),
    refetchInterval: timeframe === "5s" || timeframe === "10s" ? 500 : 1000,
    enabled:  !!symbol,
    gcTime:   0,
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

  // ── Lazy "infinite history" back-fill (scroll/zoom into the past). When the
  // visible range nears the left edge, fetch ONE batch of older bars from Alpaca
  // (ending before the oldest loaded bar) and prepend them. After the prepend
  // lightweight-charts keeps the visible TIME range, so the logical `from` jumps up
  // by the batch size; as the user keeps scrolling left the chart's range-change
  // handler re-fires and the next step loads — history grows in BACKFILL_BATCH-bar
  // steps. Guarded against concurrent / dead-end fetches.
  const loadOlderBars = useCallback(async () => {
    const sym = symbolRef.current;
    const tf  = timeframeRef.current;
    if (loadingOlderRef.current || noMoreOlderRef.current) return;
    if (tf === "5s" || tf === "10s") return; // Alpaca REST can't serve sub-minute
    const accum = accumRef.current;
    if (accum.size === 0) return;

    loadingOlderRef.current = true;
    try {
      // Oldest currently-loaded bar = the back-fill cutoff.
      let oldestSec = Infinity;
      let oldestIso = "";
      for (const [sec, b] of accum) {
        if (sec < oldestSec) { oldestSec = sec; oldestIso = b.time; }
      }
      if (!oldestIso) return;

      const older = await api.loadOlderBars(sym, tf, oldestIso, BACKFILL_BATCH);
      // The chart may have switched symbol/timeframe while the request was in
      // flight — discard the stale batch rather than poison the new accumulator.
      if (symbolRef.current !== sym || timeframeRef.current !== tf) return;
      if (!older?.length) { noMoreOlderRef.current = true; return; }

      let added = 0;
      for (const b of older) {
        const sec = toUTC(b.time) as number;
        if (!accum.has(sec)) { accum.set(sec, b); added++; }
      }
      if (added === 0) { noMoreOlderRef.current = true; return; } // reached data start

      // One render: the bar-series effect sees a changed FIRST timestamp → full
      // setData (which preserves the user's view) instead of update().
      setBars([...accum.entries()].sort((a, c) => a[0] - c[0]).map((e) => e[1]));
    } catch { /* soft-fail */ }
    finally { loadingOlderRef.current = false; }
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  return { bars, loadOlderBars };
}
