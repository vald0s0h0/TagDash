import { useEffect, useRef, type MutableRefObject } from "react";
import type { ISeriesApi, IPriceLine } from "lightweight-charts";
import { DRAFT_ENTRY_OPTS, DRAFT_SL_OPTS, DRAFT_TP_OPTS } from "@/charts/chartOptions";

function syncLine(
  series: ISeriesApi<"Candlestick"> | null,
  ref: MutableRefObject<IPriceLine | null>,
  price: number | null,
  opts: Record<string, unknown>,
) {
  if (!series) return;
  if (price == null) {
    if (ref.current) { series.removePriceLine(ref.current); ref.current = null; }
    return;
  }
  if (ref.current) {
    ref.current.applyOptions({ price });
  } else {
    ref.current = series.createPriceLine({ price, ...opts });
  }
}

export function useDraftLines(
  candleRef: MutableRefObject<ISeriesApi<"Candlestick"> | null>,
  draftEntryLineRef: MutableRefObject<IPriceLine | null>,
  draftSlLineRef: MutableRefObject<IPriceLine | null>,
  draftTpLineRef: MutableRefObject<IPriceLine | null>,
  draftEntry: number | null,
  draftSl: number | null,
  draftTp: number | null,
) {
  useEffect(() => {
    syncLine(candleRef.current, draftEntryLineRef, draftEntry, DRAFT_ENTRY_OPTS);
  }, [draftEntry]); // eslint-disable-line react-hooks/exhaustive-deps

  useEffect(() => {
    syncLine(candleRef.current, draftSlLineRef, draftSl, DRAFT_SL_OPTS);
  }, [draftSl]); // eslint-disable-line react-hooks/exhaustive-deps

  useEffect(() => {
    syncLine(candleRef.current, draftTpLineRef, draftTp, DRAFT_TP_OPTS);
  }, [draftTp]); // eslint-disable-line react-hooks/exhaustive-deps

  // Cleanup on unmount.
  const entryRef = useRef(draftEntryLineRef);
  const slRef = useRef(draftSlLineRef);
  const tpRef = useRef(draftTpLineRef);
  entryRef.current = draftEntryLineRef;
  slRef.current = draftSlLineRef;
  tpRef.current = draftTpLineRef;

  useEffect(() => {
    return () => {
      const series = candleRef.current;
      if (!series) return;
      for (const r of [entryRef.current, slRef.current, tpRef.current]) {
        if (r.current) {
          try { series.removePriceLine(r.current); } catch { /* disposed */ }
          r.current = null;
        }
      }
    };
  }, []); // eslint-disable-line react-hooks/exhaustive-deps
}
