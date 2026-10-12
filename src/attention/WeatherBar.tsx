// Market weather: how hot small caps and the whole tape are, how wide the activity
// is and how concentrated — each read against its own median at the same hour, and
// named with thresholds the trader sets.
//
// A gauge with too little history says so and shows its raw figure: an unknown
// ratio is never drawn as "normal".

import { fmtMoney, level, slotTime, type Level } from "./model";
import type { AttentionOptions, Gauge, MapView } from "./types";

function Meter({ lvl }: { lvl: Level | null }) {
  return (
    <span className="flex items-end gap-[2px]" aria-hidden>
      {[0, 1, 2, 3].map((i) => (
        <span
          key={i}
          className={lvl != null && i <= lvl ? "bg-foreground" : "bg-foreground/15"}
          style={{ width: 3, height: 5 + i * 3, borderRadius: 1 }}
        />
      ))}
    </span>
  );
}

function Cell({
  title, gauge, lvl, words, value, median, hint,
}: {
  title: string;
  gauge: Gauge;
  lvl: Level | null;
  words: [string, string, string, string];
  value: string;
  median: string | null;
  hint: string;
}) {
  return (
    <div className="flex min-w-0 items-center gap-2 rounded border border-border px-2.5 py-1.5" title={hint}>
      <Meter lvl={lvl} />
      <div className="min-w-0">
        <div className="truncate text-[10px] uppercase tracking-wide text-muted-foreground/70">{title}</div>
        <div className="flex items-baseline gap-1.5">
          <span className="text-[13px] font-semibold text-foreground">
            {lvl == null ? "n/d" : words[lvl]}
          </span>
          {gauge.ratio != null && (
            <span className="text-[11px] tabular-nums text-muted-foreground">×{gauge.ratio.toFixed(2)}</span>
          )}
        </div>
        <div className="truncate text-[10px] tabular-nums text-muted-foreground">
          {value}{median ? ` · habituel ${median}` : " · historique insuffisant"}
        </div>
      </div>
    </div>
  );
}

export function WeatherBar({ map, opts }: { map: MapView; opts: AttentionOptions }) {
  const w = map.weather;
  if (w.slot == null) {
    return (
      <div className="rounded border border-border px-2.5 py-1.5 text-[11px] text-muted-foreground">
        Météo : aucun créneau clos pour l'instant.
      </div>
    );
  }
  const when = `${slotTime(map.frame, w.slot)} ET`;
  const base = `Créneau de ${when}, comparé à la médiane de ${w.sessions} séances à la même heure.`;
  const pct = (v: number) => `${(v * 100).toFixed(0)} %`;
  return (
    <div className="grid grid-cols-2 gap-2 xl:grid-cols-4">
      <Cell
        title="Chaleur small caps"
        gauge={w.small}
        lvl={level(w.small.ratio, opts.heat_low, opts.heat_high, opts.heat_extreme)}
        words={["Froid", "Normal", "Chaud", "Brûlant"]}
        value={fmtMoney(w.small.value)}
        median={w.small.median == null ? null : fmtMoney(w.small.median)}
        hint={`Dollars échangés sur les actions < 2 Md$. ${base}`}
      />
      <Cell
        title="Chaleur marché global"
        gauge={w.global}
        lvl={level(w.global.ratio, opts.heat_low, opts.heat_high, opts.heat_extreme)}
        words={["Froid", "Normal", "Chaud", "Brûlant"]}
        value={fmtMoney(w.global.value)}
        median={w.global.median == null ? null : fmtMoney(w.global.median)}
        hint={`Dollars échangés sur tout l'univers suivi (actions et fonds). ${base}`}
      />
      <Cell
        title="Largeur"
        gauge={w.breadth}
        lvl={level(w.breadth.ratio, opts.breadth_low, opts.breadth_high)}
        words={["Étroite", "Normale", "Large", "Large"]}
        value={`${w.breadth.value.toFixed(0)} actions actives`}
        median={w.breadth.median == null ? null : w.breadth.median.toFixed(0)}
        hint={`Actions ayant échangé au moins le seuil d'activité sur le créneau. ${base}`}
      />
      <Cell
        title="Concentration"
        gauge={w.concentration}
        lvl={level(w.concentration.ratio, opts.concentration_low, opts.concentration_high)}
        words={["Dispersée", "Normale", "Concentrée", "Concentrée"]}
        value={`top 5 = ${pct(w.concentration.value)}`}
        median={w.concentration.median == null ? null : pct(w.concentration.median)}
        hint={`Part des dollars des actions captée par les cinq plus actives. ${base}`}
      />
    </div>
  );
}
