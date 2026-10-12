// Context charts of the review sidebar: the daily (every session before the traded
// day — nothing of the future) and the 5-minute chart of the day, cut at the
// replay's playhead, with the executions appearing at their exact millisecond.
// Both dressed like the trading panes (TagDash defaults, the strategy card's
// indicators for that timeframe — SMA 200/20 on Micro Pullback's daily — and the
// red dots on split days).

import { useEffect, useMemo, useRef } from "react";
import { useQuery } from "@tanstack/react-query";
import type { ISeriesApi } from "lightweight-charts";
import { api } from "@/lib/tauri";
import { useCandleMarkers } from "@/charts/useCandleMarkers";
import { useChartTheme } from "@/stores/chartThemeStore";
import type { Bar, ReviewSessionData, StrategyCard } from "@/types";
import { buildBars, visibleExecutions, type OhlcBar, type PlayFill } from "./model";
import { paneIndicators, ReviewCandleChart } from "./chart";
import { usePlayheadStore } from "./playheadStore";

const FIVE_MIN = 300;

/** `data` is the ticker's day (shared by its trades): the charts are kept while
 *  the sidebar moves from one trade to the next. */
export function ContextCharts({
  data, symbol, fills, levelsOf, card,
}: {
  data: ReviewSessionData;
  symbol: string;
  fills: PlayFill[];
  levelsOf: (trade: string) => { sl: number | null; tp: number | null };
  card: StrategyCard | null;
}) {
  const dailyEl = useRef<HTMLDivElement>(null);
  const m5El = useRef<HTMLDivElement>(null);
  const daily = useRef<ReviewCandleChart | null>(null);
  const dailyCandles = useRef<ISeriesApi<"Candlestick"> | null>(null);
  const m5 = useRef<{ rc: ReviewCandleChart } | null>(null);

  const dailyBars: OhlcBar[] = useMemo(
    () => data.bars_1d.map(([time, open, high, low, close, volume]) => ({ time, open, high, low, close, volume })),
    [data],
  );

  useEffect(() => {
    if (!dailyEl.current || !m5El.current) return;
    const d = new ReviewCandleChart(dailyEl.current, false, paneIndicators(card, "daily"), false, 120);
    d.setBars(dailyBars, true);
    daily.current = d;
    dailyCandles.current = d.candles;
    const rc = new ReviewCandleChart(m5El.current, false, paneIndicators(card, "5m"), true, 40);
    m5.current = { rc };
    return () => {
      daily.current = null;
      dailyCandles.current = null;
      m5.current = null;
      d.remove();
      rc.remove();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [data]);

  useEffect(() => {
    daily.current?.setIndicators(paneIndicators(card, "daily"));
    m5.current?.rc.setIndicators(paneIndicators(card, "5m"));
  }, [card]);

  const theme = useChartTheme();
  useEffect(() => {
    daily.current?.applyTheme(theme);
    m5.current?.rc.applyTheme(theme);
  }, [theme]);

  // Red dots on split days, as on every daily pane.
  const { data: splits } = useQuery({
    queryKey: ["split_markers", symbol],
    queryFn: () => api.getSplitMarkers(symbol),
    staleTime: 6 * 60 * 60 * 1000,
  });
  const splitMarkers = useMemo(
    () => (splits ?? []).map((m) => ({ time: m.time, color: theme.markers.split, text: m.label })),
    [splits, theme.markers.split],
  );
  const dailyAsBars: Bar[] = useMemo(
    () => dailyBars.map((b) => ({ ...b, time: new Date(b.time * 1000).toISOString(), vwap: null })),
    [dailyBars],
  );
  useCandleMarkers(dailyCandles, splitMarkers, dailyAsBars);

  // Follow the playhead.
  useEffect(() => {
    const draw = (at: number) => {
      const c = m5.current;
      if (!c) return;
      const bars = buildBars(data, FIVE_MIN, at);
      c.rc.setBars(bars);
      c.rc.exec.setData(visibleExecutions(fills, at, null, null, levelsOf), bars.map((b) => b.time));
    };
    draw(usePlayheadStore.getState().ms);
    return usePlayheadStore.subscribe((s) => draw(s.ms));
  }, [data, fills, levelsOf]);

  return (
    <div className="shrink-0 border-t border-border">
      <div className="relative h-48">
        <div ref={dailyEl} className="absolute inset-0" />
        <span className="pointer-events-none absolute left-1.5 top-1 z-10 text-[9px] uppercase text-muted-foreground">Daily</span>
      </div>
      <div className="relative h-60 border-t border-border">
        <div ref={m5El} className="absolute inset-0" />
        <span className="pointer-events-none absolute left-1.5 top-1 z-10 text-[9px] uppercase text-muted-foreground">5 min</span>
      </div>
    </div>
  );
}
