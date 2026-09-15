// Backside's own Settings panel — rendered ONLY under that strategy.
//
// Only three knobs, unlike Dislocation/Micro Pullback: Backside has no engine
// loop, so there's no tempo/budget/LLM-gate surface to expose here — just the
// Gate 1 thresholds that ARE user-configurable (institutional ownership is
// fixed at 50% in the strategy code, not exposed).
//
// Every change commits on blur and writes through to the backend's
// module-static options holder immediately, so the very next scan tick sees it
// — no restart needed (see `strategies::backside`).

import { useEffect, useState } from "react";

import { Input } from "@/components/ui/input";
import { useBacksideOptions, useSetBacksideOptions } from "@/queries/useScanner";

export function BacksidePanel() {
  const { data: options } = useBacksideOptions();
  const setOptions = useSetBacksideOptions();
  const [changeMin, setChangeMin] = useState("60");
  const [marketCapMax, setMarketCapMax] = useState("150");
  const [priceMin, setPriceMin] = useState("1");

  useEffect(() => {
    if (options) setChangeMin(String(options.change_min_pct));
  }, [options?.change_min_pct]);

  // Shown in millions: nobody types 150000000.
  useEffect(() => {
    if (options) setMarketCapMax(String(options.market_cap_max / 1_000_000));
  }, [options?.market_cap_max]);

  useEffect(() => {
    if (options) setPriceMin(String(options.price_min));
  }, [options?.price_min]);

  if (!options) return null;
  const busy = setOptions.isPending;

  function commitChangeMin() {
    const n = parseFloat(changeMin);
    if (Number.isFinite(n) && n >= 0) {
      if (n !== options!.change_min_pct) {
        setOptions.mutate({ ...options!, change_min_pct: n });
      }
    } else {
      setChangeMin(String(options!.change_min_pct));
    }
  }

  function commitMarketCapMax() {
    const millions = parseFloat(marketCapMax);
    if (Number.isFinite(millions) && millions > 0) {
      const dollars = Math.round(millions * 1_000_000);
      if (dollars !== options!.market_cap_max) {
        setOptions.mutate({ ...options!, market_cap_max: dollars });
      }
    } else {
      setMarketCapMax(String(options!.market_cap_max / 1_000_000));
    }
  }

  function commitPriceMin() {
    const n = parseFloat(priceMin);
    if (Number.isFinite(n) && n >= 0) {
      if (n !== options!.price_min) {
        setOptions.mutate({ ...options!, price_min: n });
      }
    } else {
      setPriceMin(String(options!.price_min));
    }
  }

  return (
    <div className="mt-2 space-y-2 border-t border-border/40 pt-2">
      <div>
        <div className="mb-1 text-[10px] text-muted-foreground">Changement vs clôture veille minimum</div>
        <div className="flex items-center gap-1.5">
          <Input
            type="number"
            min={0}
            step="1"
            value={changeMin}
            disabled={busy}
            onChange={(e) => setChangeMin(e.target.value)}
            onBlur={commitChangeMin}
            onKeyDown={(e) => { if (e.key === "Enter") (e.target as HTMLInputElement).blur(); }}
            className="h-6 w-16 text-xs tabular-nums"
          />
          <span className="text-[10px] text-muted-foreground/70">%</span>
        </div>
        <div className="mt-1 text-[10px] leading-snug text-muted-foreground/70">
          Écart minimum entre le prix actuel et la clôture de la veille, à la hausse.
          Valeur par défaut&nbsp;: 60&nbsp;%.
        </div>
      </div>

      <div>
        <div className="mb-1 text-[10px] text-muted-foreground">Capitalisation boursière maximale</div>
        <div className="flex items-center gap-1.5">
          <Input
            type="number"
            min={1}
            step="1"
            value={marketCapMax}
            disabled={busy}
            onChange={(e) => setMarketCapMax(e.target.value)}
            onBlur={commitMarketCapMax}
            onKeyDown={(e) => { if (e.key === "Enter") (e.target as HTMLInputElement).blur(); }}
            className="h-6 w-20 text-xs tabular-nums"
          />
          <span className="text-[10px] text-muted-foreground/70">millions $</span>
        </div>
        <div className="mt-1 text-[10px] leading-snug text-muted-foreground/70">
          Un ticker doit être EN DESSOUS de ce plafond pour être retenu (petites caps
          uniquement). Valeur par défaut&nbsp;: 150&nbsp;M$.
        </div>
      </div>

      <div>
        <div className="mb-1 text-[10px] text-muted-foreground">Prix minimum</div>
        <div className="flex items-center gap-1.5">
          <Input
            type="number"
            min={0}
            step="0.1"
            value={priceMin}
            disabled={busy}
            onChange={(e) => setPriceMin(e.target.value)}
            onBlur={commitPriceMin}
            onKeyDown={(e) => { if (e.key === "Enter") (e.target as HTMLInputElement).blur(); }}
            className="h-6 w-16 text-xs tabular-nums"
          />
          <span className="text-[10px] text-muted-foreground/70">$</span>
        </div>
        <div className="mt-1 text-[10px] leading-snug text-muted-foreground/70">
          Un ticker doit être AU-DESSUS de ce plancher pour être retenu. Valeur par
          défaut&nbsp;: 1&nbsp;$.
        </div>
      </div>

      <div className="text-[10px] leading-snug text-muted-foreground/60">
        Détention institutionnelle&nbsp;: seuil fixe à 50&nbsp;% (non modifiable ici).
        Donnée inconnue ou capitalisation inconnue&nbsp;: le ticker est retenu quand
        même.
      </div>
    </div>
  );
}
