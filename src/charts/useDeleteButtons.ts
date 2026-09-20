import { useEffect, type MutableRefObject } from "react";
import type { ISeriesApi } from "lightweight-charts";
import type { ChartAlarm } from "@/stores/chartStore";

/** Right-edge label pills tracked to a price line.
 *
 *  Two kinds ride the same loop. The user lines (SL/TP/alarms) get a "price + ✕"
 *  pill: lightweight-charts has no native delete affordance and no interactive axis
 *  tag, so the pill stands in for the (disabled) native axis label with the delete
 *  button integrated INTO it rather than floating over the chart text. The bid line
 *  gets a spread pill — same right-edge slot, no ✕, because what belongs there is
 *  the spread and a native tag can only print the line's own price.
 *
 *  This hook keeps the target price→pixel map fresh (1st effect) and repositions
 *  the pill DOM nodes every frame via requestAnimationFrame so they track their
 *  line through scroll / zoom / autoscale (2nd effect). The pill nodes live in
 *  `tagBtnRefs` (populated by the JSX ref callbacks), the target prices in
 *  `tagPriceRef`. */
export function useDeleteButtons(
  candleRef: MutableRefObject<ISeriesApi<"Candlestick"> | null>,
  tagBtnRefs: MutableRefObject<Map<string, HTMLElement>>,
  tagPriceRef: MutableRefObject<Map<string, number>>,
  slPrice: number | null,
  tpPrice: number | null,
  alarms: ChartAlarm[],
  /** Bid price the spread pill rides, or null when there is no live spread. */
  spreadPrice: number | null,
  onDeleteSl?: () => void,
  onDeleteTp?: () => void,
  onDeleteAlarm?: (id: string) => void,
) {
  const alarmsKey = alarms.map((a) => `${a.id}:${a.price}`).join(",");
  useEffect(() => {
    const m = new Map<string, number>();
    if (slPrice != null && onDeleteSl) m.set("sl", slPrice);
    if (tpPrice != null && onDeleteTp) m.set("tp", tpPrice);
    if (onDeleteAlarm) for (const a of alarms) m.set(`alarm-${a.id}`, a.price);
    if (spreadPrice != null) m.set("spread", spreadPrice);
    tagPriceRef.current = m;
  }, [slPrice, tpPrice, alarmsKey, spreadPrice, onDeleteSl, onDeleteTp, onDeleteAlarm]); // eslint-disable-line react-hooks/exhaustive-deps

  useEffect(() => {
    let raf = 0;
    const tick = () => {
      const candle = candleRef.current;
      if (candle) {
        for (const [key, node] of tagBtnRefs.current) {
          const price = tagPriceRef.current.get(key);
          const y = price != null ? candle.priceToCoordinate(price) : null;
          if (y == null) {
            node.style.display = "none";
          } else {
            node.style.display = "flex";
            node.style.top = `${y}px`;
          }
        }
      }
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  }, []);
}
