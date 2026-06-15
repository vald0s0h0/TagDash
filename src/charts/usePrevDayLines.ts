import { useEffect, type MutableRefObject } from "react";
import type { ISeriesApi, IPriceLine } from "lightweight-charts";
import type { PaneIndicator, PrevDayLevels } from "@/types";
import { PREV_DAY_OPTIONS } from "@/charts/chartOptions";

/** Previous-day reference price lines (PDC / PDH / PDL) reconciled by kind against
 *  the pane's requested previous_* indicators. The lines live on the candle series
 *  and are tracked in `prevDayLineMap` (owned by the component). Extracted verbatim
 *  from LightweightChart; same `[prevDay, prevDayKindsKey]` dependency. */
export function usePrevDayLines(
  candleRef: MutableRefObject<ISeriesApi<"Candlestick"> | null>,
  prevDayLineMap: MutableRefObject<Map<string, IPriceLine>>,
  indicators: PaneIndicator[],
  prevDay: PrevDayLevels | null | undefined,
) {
  const prevDayKindsKey = indicators
    .filter((i) => i.kind === "previous_close" || i.kind === "previous_high" || i.kind === "previous_low")
    .map((i) => i.kind)
    .join(",");
  useEffect(() => {
    const series = candleRef.current;
    if (!series) return;
    const wanted = new Map<keyof typeof PREV_DAY_OPTIONS, number>();
    if (prevDay) {
      for (const ind of indicators) {
        if (ind.kind === "previous_close") wanted.set("previous_close", prevDay.close);
        else if (ind.kind === "previous_high") wanted.set("previous_high", prevDay.high);
        else if (ind.kind === "previous_low") wanted.set("previous_low", prevDay.low);
      }
    }
    // Remove no-longer-wanted lines.
    for (const [id, line] of prevDayLineMap.current) {
      if (!wanted.has(id as keyof typeof PREV_DAY_OPTIONS)) {
        series.removePriceLine(line);
        prevDayLineMap.current.delete(id);
      }
    }
    // Create / update wanted lines.
    for (const [id, price] of wanted) {
      const opt = PREV_DAY_OPTIONS[id];
      const existing = prevDayLineMap.current.get(id);
      if (existing) {
        existing.applyOptions({ price });
      } else {
        prevDayLineMap.current.set(
          id,
          series.createPriceLine({
            price,
            color:            opt.color,
            lineWidth:        1,
            lineStyle:        opt.lineStyle,
            axisLabelVisible: true,
            title:            opt.title,
          }),
        );
      }
    }
  }, [prevDay, prevDayKindsKey]); // eslint-disable-line react-hooks/exhaustive-deps
}
