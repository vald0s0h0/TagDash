import { useEffect, type MutableRefObject } from "react";
import type { ISeriesApi, IPriceLine } from "lightweight-charts";
import type { Bar } from "@/types";
import { slOpts, tpOpts } from "@/charts/chartOptions";

/** User SL/TP price lines on the candle series: planned levels (solid) that switch
 *  to bracket-order styling (dotted) when a position is open, plus the live TP
 *  R-ratio title. The lines live in slLineRef/tpLineRef (owned by the component and
 *  also driven by the chart's click/dblclick/drag handlers), so the refs are passed
 *  in. Extracted verbatim from LightweightChart; each effect keeps its exact
 *  dependency list (notably the TP create/remove stays `[tpPrice]` only — adding
 *  `bars` was the root cause of the "TP disappears after a few seconds" bug). */
export function useSlTpLines(
  candleRef: MutableRefObject<ISeriesApi<"Candlestick"> | null>,
  slLineRef: MutableRefObject<IPriceLine | null>,
  tpLineRef: MutableRefObject<IPriceLine | null>,
  tpPriceRef: MutableRefObject<number | null>,
  slPrice: number | null,
  tpPrice: number | null,
  ordersActive: boolean,
  bars: Bar[] | undefined,
) {
  // SL price line — sync from parent state (load on startup / zone switch). When
  // the user draws via double-click the chart handler already created the line;
  // this just confirms the price (no duplicate line).
  useEffect(() => {
    const series = candleRef.current;
    if (!series) return;
    if (slPrice == null) {
      if (slLineRef.current) {
        series.removePriceLine(slLineRef.current);
        slLineRef.current = null;
      }
      return;
    }
    if (slLineRef.current) {
      slLineRef.current.applyOptions({ price: slPrice });
    } else {
      slLineRef.current = series.createPriceLine({ price: slPrice, ...slOpts(ordersActive) });
    }
  }, [slPrice]); // eslint-disable-line react-hooks/exhaustive-deps

  // TP price line — create / remove based on Rust state. deps: [tpPrice] ONLY —
  // a bars refetch must NOT trigger removal during the window between dblclick
  // (visual line created) and the Rust response (tpPrice prop update).
  useEffect(() => {
    const series = candleRef.current;
    if (!series) return;
    if (tpPrice == null) {
      if (tpLineRef.current) {
        series.removePriceLine(tpLineRef.current);
        tpLineRef.current = null;
      }
      return;
    }
    if (tpLineRef.current) {
      tpLineRef.current.applyOptions({ price: tpPrice });
    } else {
      tpLineRef.current = series.createPriceLine({ price: tpPrice, ...tpOpts(ordersActive) });
    }
  }, [tpPrice]); // eslint-disable-line react-hooks/exhaustive-deps

  // Toggle SL/TP between "planned" (solid) and "order" (dotted) styling when a
  // position opens/closes. Only color + lineStyle are touched so the TP R-ratio
  // title and the line price are preserved.
  useEffect(() => {
    if (slLineRef.current) {
      const o = slOpts(ordersActive);
      slLineRef.current.applyOptions({ color: o.color, lineStyle: o.lineStyle });
    }
    if (tpLineRef.current) {
      const o = tpOpts(ordersActive);
      tpLineRef.current.applyOptions({ color: o.color, lineStyle: o.lineStyle });
    }
  }, [ordersActive]); // eslint-disable-line react-hooks/exhaustive-deps

  // TP R-ratio title — update live as bars / slPrice change. Separate from
  // create/remove so a bars update only touches the title, never deleting the line.
  useEffect(() => {
    const line = tpLineRef.current;
    if (!line || slPrice == null || !bars?.length) return;
    const tp = tpPrice ?? tpPriceRef.current;
    if (tp == null) return;
    const entry = bars[bars.length - 1].close;
    const risk  = Math.abs(entry - slPrice);
    if (risk <= 0) return;
    line.applyOptions({ title: `TP  ${(Math.abs(tp - entry) / risk).toFixed(1)}R` });
  }, [tpPrice, slPrice, bars]); // eslint-disable-line react-hooks/exhaustive-deps
}
