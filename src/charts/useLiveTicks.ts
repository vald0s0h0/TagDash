import { useEffect, type MutableRefObject } from "react";
import { listen } from "@tauri-apps/api/event";
import type { ISeriesApi, CandlestickData, UTCTimestamp } from "lightweight-charts";
import type { Timeframe } from "@/types";
import { TF_SECONDS } from "@/charts/chartOptions";

/** Live tick stream for the displayed (focus) symbol: the backend pushes
 *  `market-tick` events; we move the current candle to the tick price (or open a
 *  new candle when the bucket flips) so the chart is truly tick-by-tick instead of
 *  waiting for a poll/close. Skipped on the daily pane (its day bar is
 *  authoritative from Alpaca). Extracted verbatim from LightweightChart; same
 *  `[symbol, timeframe]` dependency, same listener cleanup. */
export function useLiveTicks(
  candleRef: MutableRefObject<ISeriesApi<"Candlestick"> | null>,
  lastBarRef: MutableRefObject<CandlestickData | null>,
  symbol: string,
  timeframe: Timeframe,
) {
  useEffect(() => {
    if (timeframe === "daily") return;
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    listen<{ symbol: string; price: number; ts: number }>("market-tick", (e) => {
      const t = e.payload;
      if (t.symbol !== symbol) return;
      const series = candleRef.current;
      if (!series) return;
      const secs   = TF_SECONDS[timeframe] ?? 60;
      const bucket = (Math.floor(t.ts / secs) * secs) as UTCTimestamp;
      const last   = lastBarRef.current;
      const bar: CandlestickData =
        last && (last.time as number) === bucket
          ? { time: bucket, open: last.open, high: Math.max(last.high, t.price), low: Math.min(last.low, t.price), close: t.price }
          : (last && (last.time as number) > bucket)
            ? last // out-of-order tick — ignore
            : { time: bucket, open: t.price, high: t.price, low: t.price, close: t.price };
      lastBarRef.current = bar;
      try { series.update(bar); } catch { /* out-of-range guard */ }
    }).then((fn) => { if (cancelled) fn(); else unlisten = fn; });
    return () => { cancelled = true; unlisten?.(); };
  }, [symbol, timeframe]);
}
