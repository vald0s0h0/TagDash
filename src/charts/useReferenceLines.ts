import { useEffect, type MutableRefObject } from "react";
import type { ISeriesApi, IPriceLine } from "lightweight-charts";
import { ENTRY_OPTIONS, BID_ASK_OPTIONS } from "@/charts/chartOptions";

/** Informational horizontal price lines driven by live props: the average entry
 *  (faint dashed) and the live bid/ask (dashed, labelled). Each manages its own
 *  IPriceLine ref — owned by the component, nulled by the chart teardown — so the
 *  refs are passed in. Extracted verbatim from LightweightChart; same per-effect
 *  dependencies, same behaviour. */
export function useReferenceLines(
  candleRef: MutableRefObject<ISeriesApi<"Candlestick"> | null>,
  entryLineRef: MutableRefObject<IPriceLine | null>,
  bidLineRef: MutableRefObject<IPriceLine | null>,
  askLineRef: MutableRefObject<IPriceLine | null>,
  entryPrice: number | null,
  bid: number | null,
  ask: number | null,
) {
  // Entry price line.
  useEffect(() => {
    const series = candleRef.current;
    if (!series) return;
    if (entryPrice == null) {
      if (entryLineRef.current) {
        series.removePriceLine(entryLineRef.current);
        entryLineRef.current = null;
      }
      return;
    }
    if (entryLineRef.current) {
      entryLineRef.current.applyOptions({ price: entryPrice });
    } else {
      entryLineRef.current = series.createPriceLine({ price: entryPrice, ...ENTRY_OPTIONS });
    }
  }, [entryPrice]); // eslint-disable-line react-hooks/exhaustive-deps

  // Bid / Ask price lines.
  useEffect(() => {
    const series = candleRef.current;
    if (!series) return;
    if (bid != null) {
      if (bidLineRef.current) {
        bidLineRef.current.applyOptions({ price: bid });
      } else {
        bidLineRef.current = series.createPriceLine({ price: bid, ...BID_ASK_OPTIONS, title: "Bid" });
      }
    } else if (bidLineRef.current) {
      series.removePriceLine(bidLineRef.current);
      bidLineRef.current = null;
    }
    if (ask != null) {
      if (askLineRef.current) {
        askLineRef.current.applyOptions({ price: ask });
      } else {
        askLineRef.current = series.createPriceLine({ price: ask, ...BID_ASK_OPTIONS, title: "Ask" });
      }
    } else if (askLineRef.current) {
      series.removePriceLine(askLineRef.current);
      askLineRef.current = null;
    }
  }, [bid, ask]); // eslint-disable-line react-hooks/exhaustive-deps
}
