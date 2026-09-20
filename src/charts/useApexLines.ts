import { useEffect, useRef, type MutableRefObject } from "react";
import { listen } from "@tauri-apps/api/event";
import type { CandlestickData, IPriceLine, ISeriesApi } from "lightweight-charts";
import { apexLineDefs, apexLineStyle } from "@/charts/chartOptions";

/** What an armed Apex draws on a pane showing its ticker. */
export interface ApexLinesSpec {
  /** Stop distance ($) — the lines sit at price ± the distances. */
  slDistance: number;
  /** Target distance ($), null under the dynamic take profit (no target price). */
  tpDistance: number | null;
  long: boolean;
  short: boolean;
  /** False while a distance is under the minimum — drawn dimmed, nothing fires. */
  tradeable: boolean;
}

/** The armed-Apex lines, glued to the last price. They follow `market-tick`
 *  directly through `applyOptions` (no React render per tick), so they move with
 *  the tape at the same cadence as the forming candle. `spec` null removes them. */
export function useApexLines(
  candleRef: MutableRefObject<ISeriesApi<"Candlestick"> | null>,
  lastBarRef: MutableRefObject<CandlestickData | null>,
  symbol: string,
  spec: ApexLinesSpec | null,
) {
  const linesRef = useRef(new Map<string, IPriceLine>());
  const priceRef = useRef<number | null>(null);
  const specRef = useRef(spec);
  specRef.current = spec;

  const remove = () => {
    const series = candleRef.current;
    for (const line of linesRef.current.values()) {
      if (series) {
        try { series.removePriceLine(line); } catch { /* disposed */ }
      }
    }
    linesRef.current.clear();
  };

  /** Create, move or drop each line for `price`; restyle when `restyle`. */
  const place = (price: number, restyle: boolean) => {
    const series = candleRef.current;
    const s = specRef.current;
    if (!series || !s) return;
    const defs = apexLineDefs(s);
    const style = apexLineStyle(s.tradeable);
    const wanted = new Set(defs.map((d) => d.key));
    for (const [key, line] of linesRef.current) {
      if (wanted.has(key)) continue;
      try { series.removePriceLine(line); } catch { /* disposed */ }
      linesRef.current.delete(key);
    }
    for (const def of defs) {
      const at = price + def.offset;
      const existing = linesRef.current.get(def.key);
      if (existing) {
        existing.applyOptions(restyle ? { ...style, title: def.title, price: at } : { price: at });
      } else {
        linesRef.current.set(def.key, series.createPriceLine({ ...style, title: def.title, price: at }));
      }
    }
  };

  const armed = spec != null;

  // Arm / disarm / new distances or style.
  useEffect(() => {
    if (!spec) { remove(); return; }
    const price = priceRef.current ?? lastBarRef.current?.close ?? null;
    if (price != null) place(price, true);
  }, [armed, spec?.slDistance, spec?.tpDistance, spec?.long, spec?.short, spec?.tradeable]); // eslint-disable-line react-hooks/exhaustive-deps

  // Follow the price while armed.
  useEffect(() => {
    if (!armed) return;
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    listen<{ symbol: string; price: number }>("market-tick", (e) => {
      if (e.payload.symbol !== symbol) return;
      const first = linesRef.current.size === 0;
      priceRef.current = e.payload.price;
      place(e.payload.price, first);
    }).then((fn) => { if (cancelled) fn(); else unlisten = fn; });
    return () => { cancelled = true; unlisten?.(); priceRef.current = null; };
  }, [armed, symbol]); // eslint-disable-line react-hooks/exhaustive-deps

  // A ticker swap in the pane drops the old lines.
  useEffect(() => () => remove(), [symbol]); // eslint-disable-line react-hooks/exhaustive-deps
}
