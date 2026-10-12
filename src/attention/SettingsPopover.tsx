// Thresholds of the attention screen: what puts a ticker on the map, and where the
// weather gauges change name. Saved backend-side (the engine and « Revue » read the
// same values); the number of ribbons drawn is a display preference of this screen.

import { useEffect, useState } from "react";
import { attentionApi } from "./api";
import { useAttentionPrefs } from "./store";
import type { AttentionOptions } from "./types";

const FIELDS: { key: keyof AttentionOptions; label: string; step: number; hint: string }[] = [
  { key: "min_rel",            label: "Activité relative mini (×)", step: 0.5,  hint: "Atteinte une fois, elle garde le ticker en mémoire" },
  { key: "min_move_pct",       label: "Variation mini (%)",         step: 0.5,  hint: "Écart à la clôture précédente, en valeur absolue" },
  { key: "min_dollars_5m",     label: "Créneau qui compte ($)",     step: 5000, hint: "En dessous, un créneau ne fixe ni pic ni extrême" },
  { key: "baseline_floor",     label: "Plancher de médiane ($)",    step: 1000, hint: "Dénominateur minimal : pas de ratio infini" },
  { key: "small_cap_max",      label: "Seuil small cap ($)",        step: 1e8,  hint: "Micro/small en dessous, mid/big à partir de" },
  { key: "heat_low",           label: "Chaleur : froid sous (×)",   step: 0.05, hint: "Ratio à la médiane du même horaire" },
  { key: "heat_high",          label: "Chaleur : chaud dès (×)",    step: 0.1,  hint: "" },
  { key: "heat_extreme",       label: "Chaleur : brûlant dès (×)",  step: 0.1,  hint: "" },
  { key: "breadth_low",        label: "Largeur : étroite sous (×)", step: 0.05, hint: "" },
  { key: "breadth_high",       label: "Largeur : large dès (×)",    step: 0.05, hint: "" },
  { key: "concentration_low",  label: "Concentration : dispersée sous (×)", step: 0.05, hint: "" },
  { key: "concentration_high", label: "Concentration : concentrée dès (×)", step: 0.05, hint: "" },
];

export function SettingsPopover({
  opts, onSaved, onClose,
}: {
  opts: AttentionOptions;
  onSaved: (o: AttentionOptions) => void;
  onClose: () => void;
}) {
  const [draft, setDraft] = useState<AttentionOptions>(opts);
  const [error, setError] = useState<string | null>(null);
  const maxTraces = useAttentionPrefs((s) => s.maxTraces);
  const setMaxTraces = useAttentionPrefs((s) => s.setMaxTraces);
  useEffect(() => setDraft(opts), [opts]);

  const save = async () => {
    try {
      onSaved(await attentionApi.setOptions(draft));
      onClose();
    } catch (e) {
      setError(String(e));
    }
  };

  return (
    <div className="absolute right-3 top-11 z-30 w-[360px] rounded-md border border-border bg-zinc-900 p-3 shadow-2xl">
      <div className="mb-2 text-xs font-semibold text-foreground">Seuils — Attention du marché</div>
      <div className="grid grid-cols-[1fr_110px] items-center gap-x-2 gap-y-1.5">
        {FIELDS.map((f) => (
          <label key={f.key} className="contents" title={f.hint}>
            <span className="text-[11px] text-muted-foreground">{f.label}</span>
            <input
              type="number"
              step={f.step}
              value={draft[f.key]}
              onChange={(e) => setDraft({ ...draft, [f.key]: Number(e.target.value) })}
              className="rounded border border-border bg-background px-1.5 py-0.5 text-right text-[11px] tabular-nums text-foreground outline-none focus:border-ring"
            />
          </label>
        ))}
        <span className="text-[11px] text-muted-foreground" title="Au-delà, les tickers restent visibles par leur tête seule">
          Trajectoires tracées (max)
        </span>
        <input
          type="number"
          min={5}
          max={120}
          step={5}
          value={maxTraces}
          onChange={(e) => setMaxTraces(Math.max(5, Math.min(120, Number(e.target.value) || 40)))}
          className="rounded border border-border bg-background px-1.5 py-0.5 text-right text-[11px] tabular-nums text-foreground outline-none focus:border-ring"
        />
      </div>
      {error && <div className="mt-2 text-[11px] text-red-400">{error}</div>}
      <div className="mt-3 flex justify-end gap-2">
        <button type="button" onClick={onClose} className="rounded px-2 py-1 text-[11px] text-muted-foreground hover:bg-accent">
          Annuler
        </button>
        <button type="button" onClick={save} className="rounded bg-foreground px-2.5 py-1 text-[11px] font-medium text-background hover:opacity-90">
          Enregistrer
        </button>
      </div>
    </div>
  );
}
