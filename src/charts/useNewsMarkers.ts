import { useEffect, type MutableRefObject } from "react";
import { useQuery } from "@tanstack/react-query";
import type { Bar, Timeframe } from "@/types";
import { api } from "@/lib/tauri";
import type { NewsPrimitive, NewsMark } from "@/charts/newsPrimitive";
import { toUTC } from "@/charts/chartOptions";
import { useDislocationScores } from "@/queries/useScanner";

/** Index of the largest bar time (sec) ≤ `t` — the bar that was forming when the
 *  news hit. Returns -1 when `t` predates every loaded bar (off the back-filled
 *  range → the pastille appears once the user scrolls to it). Bar times ascending. */
function barIndexAtOrBefore(barTimes: number[], t: number): number {
  if (barTimes.length === 0 || t < barTimes[0]) return -1;
  let lo = 0, hi = barTimes.length - 1, ans = 0;
  while (lo <= hi) {
    const mid = (lo + hi) >> 1;
    if (barTimes[mid] <= t) { ans = mid; lo = mid + 1; }
    else hi = mid - 1;
  }
  return ans;
}

/** News pastilles for a symbol — one small dot per loaded bar that carried a
 *  headline, at the bottom of the pane (over the volume). One query per symbol
 *  (timeframe-agnostic, deduped by react-query across panes). Daily panes get one
 *  dot per day; intraday panes place the single per-bar dot at the PRECISE publish
 *  moment (interpolated between the bar and the next). One pastille marks one OR
 *  more headlines in a bar — they are never stacked.
 *
 *  THE NUMBER UNDER THE DOT is Dislocation's news third — what the LLM NEWS
 *  SCORING pass made of that headline, decayed by its age. It is attached to the
 *  LAST pastille at or before the verdict's market time, because that is the
 *  headline the model was actually shown. It comes from `micro_quality_scores`, so
 *  it survives a restart and a re-entry into the same replay; a headline no model
 *  has judged keeps a bare dot, which is not the same thing as a score of zero. */
export function useNewsMarkers(
  newsPrimRef: MutableRefObject<NewsPrimitive | null>,
  symbol: string,
  bars: Bar[] | undefined,
  timeframe: Timeframe,
) {
  const { data: news } = useQuery({
    queryKey: ["news_markers", symbol],
    queryFn:  () => api.getNewsMarkers(symbol),
    enabled:  !!symbol,
    staleTime: 5 * 60 * 1000,
  });
  // Deduped by react-query with the pane overlay's own use of the same key.
  const { bySymbol: quality } = useDislocationScores();
  const verdict  = symbol ? quality[symbol] ?? null : null;
  const newsScore = verdict?.news_score ?? null;
  const scoredAt  = verdict?.scored_at ? Date.parse(verdict.scored_at) / 1000 : null;

  useEffect(() => {
    const prim = newsPrimRef.current;
    if (!prim) return;
    if (!bars?.length || !news?.length) { prim.setData([]); return; }
    const barTimes = bars.map((b) => toUTC(b.time) as number);
    const isDaily = timeframe === "daily";

    // One pastille per bar carrying news. Intraday positions it at the precise
    // publish moment of the EARLIEST headline in that bar (smallest fraction).
    const byBar = new Map<number, NewsMark>();
    for (const n of news) {
      const i = barIndexAtOrBefore(barTimes, n.time);
      if (i < 0) continue; // predates the loaded range
      const t0 = barTimes[i];
      const t1 = i + 1 < barTimes.length ? barTimes[i + 1] : null;
      let frac = 0;
      if (!isDaily && t1 != null && t1 > t0) {
        frac = Math.min(1, Math.max(0, (n.time - t0) / (t1 - t0)));
      }
      const existing = byBar.get(t0);
      if (!existing || frac < existing.frac) byBar.set(t0, { t0, t1, frac });
    }

    const marks = [...byBar.values()].sort((a, b) => a.t0 - b.t0 || a.frac - b.frac);

    // The score belongs to the headline the model read: the newest one published at
    // or before the verdict. Attaching it to every dot would claim the model judged
    // headlines it never saw.
    if (newsScore != null && scoredAt != null) {
      let target = -1;
      for (const n of news) {
        if (n.time > scoredAt) continue;
        const i = barIndexAtOrBefore(barTimes, n.time);
        if (i < 0) continue;
        const at = marks.findIndex((m) => m.t0 === barTimes[i]);
        if (at >= 0 && at > target) target = at;
      }
      if (target >= 0) marks[target] = { ...marks[target], score: newsScore };
    }

    prim.setData(marks);
  }, [news, bars, timeframe, newsScore, scoredAt]); // eslint-disable-line react-hooks/exhaustive-deps
}
