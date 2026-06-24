import { useEffect, type MutableRefObject } from "react";
import { useQuery } from "@tanstack/react-query";
import type { Bar } from "@/types";
import { api } from "@/lib/tauri";
import type { NewsPrimitive } from "@/charts/newsPrimitive";
import { toUTC } from "@/charts/chartOptions";

/** Largest bar time (sec) ≤ `t` — the bar that was forming when the news hit.
 *  Null when `t` predates every loaded bar (off the back-filled range → appears
 *  once scrolled to). Bar times are ascending. */
function snapToBar(barTimes: number[], t: number): number | null {
  if (barTimes.length === 0 || t < barTimes[0]) return null;
  let lo = 0, hi = barTimes.length - 1, ans = barTimes[0];
  while (lo <= hi) {
    const mid = (lo + hi) >> 1;
    if (barTimes[mid] <= t) { ans = barTimes[mid]; lo = mid + 1; }
    else hi = mid - 1;
  }
  return ans;
}

/** News pastilles for a symbol — one small dot per loaded bar that carried a
 *  headline, at the bottom of the pane (over the volume). One query per symbol
 *  (timeframe-agnostic, deduped by react-query across panes); each news timestamp
 *  is snapped to the bar containing it, so it works on intraday AND daily panes. */
export function useNewsMarkers(
  newsPrimRef: MutableRefObject<NewsPrimitive | null>,
  symbol: string,
  bars: Bar[] | undefined,
) {
  const { data: news } = useQuery({
    queryKey: ["news_markers", symbol],
    queryFn:  () => api.getNewsMarkers(symbol),
    enabled:  !!symbol,
    staleTime: 5 * 60 * 1000,
  });

  useEffect(() => {
    const prim = newsPrimRef.current;
    if (!prim) return;
    if (!bars?.length || !news?.length) { prim.setData([]); return; }
    const barTimes = bars.map((b) => toUTC(b.time) as number);
    const set = new Set<number>();
    for (const n of news) {
      const snapped = snapToBar(barTimes, n.time);
      if (snapped != null) set.add(snapped);
    }
    prim.setData([...set].sort((a, b) => a - b));
  }, [news, bars]); // eslint-disable-line react-hooks/exhaustive-deps
}
