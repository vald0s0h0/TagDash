// Dislocation's own Settings panel — rendered ONLY under that strategy.
//
// These knobs are specific to this engine (which tempos to rank on, the score a
// candidate must reach, how many model calls it may spend) and are deliberately not
// a generic per-strategy channel. Dislocation has its own panel next door with a
// completely different set: the two engines answer different questions.
//
// Every change commits immediately and the engine re-reads the whole struct on its
// next one-second tick, so a toggle takes effect without a restart.

import { useEffect, useState } from "react";

import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import {
  useDislocationOptions,
  useDislocationAttention,
  useSetDislocationOptions,
} from "@/queries/useScanner";
import type { DislocationOptions } from "@/types";

/** The six detection tempos, in the order the engine evaluates them. Labels match
 *  what the alert text prints ("10s" … "5m"), so a badge on screen and a checkbox
 *  here are recognisably the same thing. */
const TEMPOS: { key: keyof DislocationOptions; label: string }[] = [
  { key: "tempo_10s", label: "10s" },
  { key: "tempo_20s", label: "20s" },
  { key: "tempo_40s", label: "40s" },
  { key: "tempo_1m", label: "1m" },
  { key: "tempo_2m", label: "2m" },
  { key: "tempo_5m", label: "5m" },
];

function Row({
  checked,
  disabled,
  label,
  hint,
  indent,
  onChange,
}: {
  checked: boolean;
  disabled?: boolean;
  label: string;
  hint?: string;
  indent?: boolean;
  onChange: (v: boolean) => void;
}) {
  return (
    <div className={`flex items-start gap-2 ${indent ? "pl-5" : ""}`}>
      <Switch
        checked={checked}
        disabled={disabled}
        onCheckedChange={onChange}
        className="mt-0.5"
      />
      <div className="min-w-0">
        <div className={`text-[11px] ${disabled ? "text-muted-foreground/50" : "text-foreground"}`}>
          {label}
        </div>
        {hint && (
          <div className="text-[10px] leading-snug text-muted-foreground/70">{hint}</div>
        )}
      </div>
    </div>
  );
}

export function DislocationPanel() {
  const { data: options } = useDislocationOptions();
  const setOptions = useSetDislocationOptions();
  const { data: attention = [] } = useDislocationAttention();
  const [threshold, setThreshold] = useState("70");
  const [priceMin, setPriceMin] = useState("1");
  const [priceMax, setPriceMax] = useState("50");
  const [floatMax, setFloatMax] = useState("50");
  const [budget, setBudget] = useState("3");

  useEffect(() => {
    if (options) setThreshold(String(options.alert_score_threshold));
  }, [options?.alert_score_threshold]);

  useEffect(() => {
    if (options) {
      setPriceMin(String(options.price_min));
      setPriceMax(String(options.price_max));
    }
  }, [options?.price_min, options?.price_max]);

  // Shown in millions: nobody types 50000000, and the field is read at a glance
  // next to the price band.
  useEffect(() => {
    if (options) setFloatMax(String(options.float_max / 1_000_000));
  }, [options?.float_max]);

  useEffect(() => {
    if (options) setBudget(String(options.llm_budget_per_5min));
  }, [options?.llm_budget_per_5min]);

  if (!options) return null;
  const busy = setOptions.isPending;
  const aplus = options.quality_aplus;

  function patch(partial: Partial<DislocationOptions>) {
    setOptions.mutate({ ...options!, ...partial });
  }

  function commitThreshold() {
    const n = parseFloat(threshold);
    // The backend clamps too; this just stops a typo from being sent at all.
    if (Number.isFinite(n) && n >= 0 && n <= 100) {
      if (n !== options!.alert_score_threshold) patch({ alert_score_threshold: n });
    } else {
      setThreshold(String(options!.alert_score_threshold));
    }
  }

  function commitPriceBand() {
    const lo = parseFloat(priceMin);
    const hi = parseFloat(priceMax);
    if (Number.isFinite(lo) && Number.isFinite(hi) && lo > 0 && hi > lo) {
      if (lo !== options!.price_min || hi !== options!.price_max) {
        patch({ price_min: lo, price_max: hi });
      }
    } else {
      setPriceMin(String(options!.price_min));
      setPriceMax(String(options!.price_max));
    }
  }

  function commitFloatMax() {
    const millions = parseFloat(floatMax);
    if (Number.isFinite(millions) && millions > 0) {
      const shares = Math.round(millions * 1_000_000);
      if (shares !== options!.float_max) patch({ float_max: shares });
    } else {
      setFloatMax(String(options!.float_max / 1_000_000));
    }
  }

  function commitBudget() {
    const n = parseInt(budget, 10);
    if (Number.isFinite(n) && n >= 1 && n <= 30) {
      if (n !== options!.llm_budget_per_5min) patch({ llm_budget_per_5min: n });
    } else {
      setBudget(String(options!.llm_budget_per_5min));
    }
  }

  const activeTempos = TEMPOS.filter((t) => options[t.key] as boolean).length;

  return (
    <div className="mt-2 space-y-2 border-t border-border/40 pt-2">
      <div>
        <div className="mb-1 text-[10px] text-muted-foreground">Fourchette de prix actuelle</div>
        <div className="flex items-center gap-1.5">
          <span className="text-[10px] text-muted-foreground/70">$</span>
          <Input
            type="number"
            min={0.01}
            step="0.01"
            value={priceMin}
            disabled={busy}
            onChange={(e) => setPriceMin(e.target.value)}
            onBlur={commitPriceBand}
            onKeyDown={(e) => { if (e.key === "Enter") (e.target as HTMLInputElement).blur(); }}
            className="h-6 w-20 text-xs tabular-nums"
          />
          <span className="text-[10px] text-muted-foreground/70">à $</span>
          <Input
            type="number"
            min={0.02}
            step="0.01"
            value={priceMax}
            disabled={busy}
            onChange={(e) => setPriceMax(e.target.value)}
            onBlur={commitPriceBand}
            onKeyDown={(e) => { if (e.key === "Enter") (e.target as HTMLInputElement).blur(); }}
            className="h-6 w-20 text-xs tabular-nums"
          />
        </div>
        <div className="mt-1 text-[10px] leading-snug text-muted-foreground/70">
          Filtre unique sur le dernier prix du flux. Valeur par défaut&nbsp;: 1–50&nbsp;$.
        </div>
      </div>

      {/* ── Float ceiling ── */}
      <div>
        <div className="mb-1 text-[10px] text-muted-foreground">Float maximum</div>
        <div className="flex items-center gap-1.5">
          <Input
            type="number"
            min={0.1}
            step="1"
            value={floatMax}
            disabled={busy}
            onChange={(e) => setFloatMax(e.target.value)}
            onBlur={commitFloatMax}
            onKeyDown={(e) => { if (e.key === "Enter") (e.target as HTMLInputElement).blur(); }}
            className="h-6 w-20 text-xs tabular-nums"
          />
          <span className="text-[10px] text-muted-foreground/70">millions d'actions</span>
        </div>
        <div className="mt-1 text-[10px] leading-snug text-muted-foreground/70">
          Valeur par défaut&nbsp;: 50&nbsp;M. Un float <strong>connu</strong> au-dessus exclut
          le ticker&nbsp;; un float inconnu reste admis et l'alerte porte la mention
          FLOAT_INCONNU.
        </div>
      </div>

      {/* ── Issuer filters ── */}
      <div className="space-y-1.5 border-t border-border/40 pt-2">
        <Row
          checked={options.hot_industry_only}
          disabled={busy}
          label="Seulement les industries porteuses"
          hint="Biotech/pharma, spatial/aéronautique/défense, crypto, quantique, cannabis, IA. Classement sur le titre SIC de la SEC, avec repli sur mots-clés. Une industrie jamais collectée n'est PAS porteuse : le ticker est exclu lui aussi."
          onChange={(v) => patch({ hot_industry_only: v })}
        />
        <Row
          checked={options.exclude_risk_domicile}
          disabled={busy}
          label="Exclure les juridictions à risque"
          hint="Chine, Hong Kong, Îles Caïmans, Îles Vierges britanniques — les quatre domiciliations surreprésentées dans les schémas de pump & dump sur small caps US. Décochée, ces tickers sont surveillés exactement comme les autres : le score n'a aucun terme de juridiction. Une domiciliation inconnue n'est jamais exclue."
          onChange={(v) => patch({ exclude_risk_domicile: v })}
        />
      </div>

      <div className="space-y-1 border-t border-border/40 pt-2">
        <div className="flex items-baseline justify-between gap-2">
          <span className="text-[10px] text-muted-foreground">Priorité marché — Top 10 par tempo</span>
          {attention[0] && (
            <span className="text-[9px] text-muted-foreground/60">
              marché {new Date(attention[0].market_time).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" })}
            </span>
          )}
        </div>
        {TEMPOS.map(({ key, label }) => {
          if (!(options[key] as boolean)) return null;
          const secs = label.endsWith("m") ? parseInt(label) * 60 : parseInt(label);
          const rows = attention.filter((r) => r.tempo_secs === secs).sort((a, b) => a.rank - b.rank);
          return (
            <div key={`attention-${label}`} className="flex items-start gap-1.5">
              <span className="w-6 shrink-0 pt-0.5 text-[9px] tabular-nums text-muted-foreground/60">{label}</span>
              <div className="flex min-w-0 flex-wrap gap-1">
                {rows.length === 0 ? (
                  <span className="text-[9px] text-muted-foreground/40">aucune donnée</span>
                ) : rows.map((r) => (
                  <span
                    key={`${label}-${r.symbol}`}
                    title={`Attention ${r.attention_score.toFixed(0)} · RVOL tempo ${r.tempo_rvol != null ? `${r.tempo_rvol.toFixed(1)}× (${r.baseline_windows} fenêtres)` : "baseline en formation"} · concentration ${r.dominance_z.toFixed(1)}σ (indicative) · ${r.evidence_ok ? "expansion confirmée" : "bruit/activité insuffisante"}${r.already_alerted ? " · déjà alerté" : ""}`}
                    className={`rounded border px-1 py-0.5 text-[9px] tabular-nums ${
                      r.evidence_ok && (
                        (r.tempo_rvol != null && r.tempo_rvol >= 5 && r.flow_rank >= 0.8)
                        || (r.tempo_rvol == null && r.attention_score >= 85 && r.flow_rank >= 0.9 && r.range_rank >= 0.9)
                      )
                        ? "border-amber-500/70 bg-amber-900/40 text-amber-200"
                        : "border-border/60 text-muted-foreground/70"
                    }`}
                  >
                    {r.rank}.{r.symbol} {r.attention_score.toFixed(0)}
                    {r.tempo_rvol != null ? ` · ${r.tempo_rvol.toFixed(1)}×` : " · init"}
                    {r.already_alerted ? "↻" : ""}
                  </span>
                ))}
              </div>
            </div>
          );
        })}
        <div className="text-[9px] leading-snug text-muted-foreground/60">
          Ambre = candidat technique, pas alerte garantie : expansion réelle, RVOL tempo ≥ 5× et flux dans le top 20 % du marché courant. A+ peut ensuite être en cours ou le rejeter. Deux tempos doivent normalement confirmer.
        </div>
      </div>

      {/* ── Detection tempos ── */}
      <div>
        <div className="mb-1 flex items-baseline gap-2">
          <span className="text-[10px] text-muted-foreground">Tempos actifs</span>
          {activeTempos === 0 && (
            <span className="text-[10px] text-amber-400">
              aucun tempo — la stratégie ne détecte rien
            </span>
          )}
        </div>
        <div className="flex flex-wrap gap-1.5">
          {TEMPOS.map(({ key, label }) => {
            const on = options[key] as boolean;
            return (
              <button
                key={key}
                type="button"
                disabled={busy}
                onClick={() => patch({ [key]: !on } as Partial<DislocationOptions>)}
                className={`h-6 rounded border px-2 text-[11px] tabular-nums transition-colors disabled:opacity-50 ${
                  on
                    ? "border-amber-500/70 bg-amber-900/40 text-amber-300"
                    : "border-border bg-transparent text-muted-foreground/60"
                }`}
              >
                {label}
              </button>
            );
          })}
        </div>
        <div className="mt-1 text-[10px] leading-snug text-muted-foreground/70">
          Aucun seuil dur sur un tempo : chaque ticker du Top 10 de chaque tempo est
          noté, et c'est le score qui décide. Les tempos confirmés sont tous affichés
          sur l'alerte.
        </div>
      </div>

      {/* ── Score ── */}
      <div className="space-y-1.5 border-t border-border/40 pt-2">
        <div className="text-[11px] font-medium">Score</div>
        <div className="text-[10px] leading-snug text-muted-foreground/70">
          Trois tiers, rien d'autre : <strong>RVOL</strong> (le ticker contre son
          propre passé), <strong>Attention</strong> (le ticker contre le marché du
          moment) et <strong>News</strong> (le LLM NEWS SCORING, décoté par l'âge du
          titre). 100 sur un tiers = un niveau atteint environ deux fois par semaine.
          Aucun bonus ni pénalité en points.
        </div>

        <div className="flex items-center gap-1.5">
          <span className="text-[10px] text-muted-foreground">Seuil d'alerte</span>
          <Input
            type="number"
            min={0}
            max={100}
            step="1"
            value={threshold}
            disabled={busy}
            onChange={(e) => setThreshold(e.target.value)}
            onBlur={commitThreshold}
            onKeyDown={(e) => {
              if (e.key === "Enter") (e.target as HTMLInputElement).blur();
            }}
            className="h-6 w-14 text-xs tabular-nums"
          />
          <span className="text-[10px] text-muted-foreground/70">
            moyenne des trois tiers
          </span>
        </div>
        <div className="text-[10px] leading-snug text-muted-foreground/70">
          Sans news dans la fenêtre de 20 min, le tiers news vaut zéro : le candidat
          plafonne à 66 quelle que soit la violence de la bande.
        </div>

        <div className="flex items-center gap-1.5">
          <span className="text-[10px] text-muted-foreground">Budget LLM</span>
          <Input
            type="number"
            min={1}
            max={30}
            step="1"
            value={budget}
            disabled={busy}
            onChange={(e) => setBudget(e.target.value)}
            onBlur={commitBudget}
            onKeyDown={(e) => { if (e.key === "Enter") (e.target as HTMLInputElement).blur(); }}
            className="h-6 w-14 text-xs tabular-nums"
          />
          <span className="text-[10px] text-muted-foreground/70">candidats / 5 min</span>
        </div>
        <div className="text-[10px] leading-snug text-muted-foreground/70">
          Les candidats sont <strong>classés</strong> sur RVOL + attention, et le budget
          est dépensé du haut de la liste. Ce qui ne rentre pas reste éligible au tick
          suivant — rien n'est perdu, c'est différé derrière mieux classé. Un candidat
          sans news fraîche ne coûte aucun appel.
        </div>

        <Row
          checked={options.news_watch}
          disabled={busy}
          label="Veille news continue"
          hint="Surveille les news après la détection et re-note dès qu'un titre tombe. Sans clés Alpaca news, repli Massive toutes les 5 min (source Massive actualisée chaque heure)."
          onChange={(v) => patch({ news_watch: v })}
        />
        <Row
          checked={aplus}
          disabled={busy}
          label="Qualité A+ — deuxième passe LLM"
          hint="Désactivée par défaut, et c'est désormais tout ce que recouvre « A+ ». Deuxième avis complet (SEC, comptes, dilution), exécuté même si la passe news refuse, et sa note REMPLACE celle de la passe news dans le tiers News. Jusqu'à 2 min réelles, même en replay accéléré. Son barème n'a pas encore été réécrit pour la nouvelle échelle."
          onChange={(v) => patch({ quality_aplus: v })}
        />
        <Row
          checked={options.alert_before_score}
          disabled={busy}
          label="Alerter sans attendre le score"
          hint="Publie dès que l'impulsion est confirmée, puis met l'alerte à jour. Plus réactif, plus bruyant."
          onChange={(v) => patch({ alert_before_score: v })}
        />
      </div>

      {/* ── Phone push. Only what clears the score threshold ever rings. ── */}
      <div className="space-y-1.5 border-t border-border/40 pt-2">
        <Row
          checked={options.ntfy_alerts}
          disabled={busy}
          label="Alerte téléphone (ntfy)"
          hint="Priorité maximale : sonnerie + vibreur. Ne sonne que pour une alerte retenue — jamais pour une alerte rétrogradée sous le seuil."
          onChange={(v) => patch({ ntfy_alerts: v })}
        />
        {options.ntfy_alerts && (
          <div className="pl-5 text-[10px] leading-snug text-muted-foreground/70">
            Topic et token dans <strong>Réglages → API Keys</strong>, avec un bouton de
            test. Le flash lumineux s'active côté iPhone (Accessibilité → Flash LED pour
            alertes) : aucune notification ne peut le déclencher à distance.
          </div>
        )}
      </div>
    </div>
  );
}
