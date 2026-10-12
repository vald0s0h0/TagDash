import { useEffect, type MutableRefObject } from "react";
import type { Bar, Timeframe } from "@/types";
import { toUTC } from "@/charts/chartOptions";
import { sessionMarks } from "@/charts/chartDefaults";
import type { SessionBandsPrimitive } from "@/charts/sessionBandsPrimitive";

/** Per-bar session marks for the backdrop primitive: which NY session each bar
 *  belongs to (pre / regular / post) and where a new NY calendar day starts.
 *  The primitive turns those into solid extended-hours zones + the day-change
 *  separator; colours and the separator toggle come from the theme, read at draw
 *  time. Intraday only — a daily bar spans a whole day, so there is nothing to
 *  shade or separate. */
export function useSessionShading(
  primRef: MutableRefObject<SessionBandsPrimitive | null>,
  bars: Bar[] | undefined,
  timeframe: Timeframe,
) {
  useEffect(() => {
    const prim = primRef.current;
    if (!prim) return;
    if (timeframe === "daily" || !bars?.length) {
      prim.setData([]);
      return;
    }
    prim.setData(sessionMarks(bars.map((b) => toUTC(b.time) as number)));
  }, [bars, timeframe]);
}
