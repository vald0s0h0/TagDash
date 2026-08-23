// Micro Pullback's own Settings panel — rendered ONLY under that strategy.
//
// These knobs are specific to this engine (which detection tempos to run, whether to
// spend an LLM call before alerting, the score a candidate must reach) and no other
// strategy has an equivalent, so there is deliberately no generic per-strategy
// options channel behind this. The other four strategies keep exactly what they had:
// an on/off Switch and StrategyRiskConfig.
//
// Every change commits immediately and the engine re-reads the whole struct on its
// next one-second tick, so a toggle takes effect without a restart.

import { useEffect, useState } from "react";

import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import {
  useMicroPullbackOptions,
  useMicroPullbackAttention,
  useSetMicroPullbackOptions,
} from "@/queries/useScanner";
import type { MicroPullbackOptions } from "@/types";

/** The six detection tempos, in the order the engine evaluates them. Labels match
 *  what the alert text prints ("10s" … "5m"), so a badge on screen and a checkbox
 *  here are recognisably the same thing. */
const TEMPOS: { key: keyof MicroPullbackOptions; label: string }[] = [
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

export function MicroPullbackPanel() {
  const { data: options } = useMicroPullbackOptions();
  const setOptions = useSetMicroPullbackOptions();
  const { data: attention = [] } = useMicroPullbackAttention();
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

  function patch(partial: Partial<MicroPullbackOptions>) {
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
          hint="Chine, Hong Kong, Îles Caïmans, Îles Vierges britanniques — les quatre domiciliations surreprésentées dans les schémas de pump & dump sur small caps US. Décochée, ces tickers restent surveillés avec une pénalité de −25. Une domiciliation inconnue n'est jamais exclue."
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
                onClick={() => patch({ [key]: !on } as Partial<MicroPullbackOptions>)}
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
          Chaque tempo exige un RVOL robuste ≥ 5× et une participation dans les 20 %
          supérieurs du marché courant. Aucun veto de direction ou de peloton. Les
          tempos confirmés sont tous affichés sur l'alerte.
        </div>
      </div>

      {/* ── Qualité A+ ── */}
      <div className="space-y-1.5 border-t border-border/40 pt-2">
        <Row
          checked={aplus}
          disabled={busy}
          label="Qualité A+ — Score Breaking"
          hint="Un candidat n'atteint le LLM que s'il a une news fraîche, un RVOL ≥ 5× et une attention nettement détachée du marché. Le modèle note alors la NEWS seule (Score Breaking 0–100) ; industrie porteuse, juridiction, RVOL exceptionnel et profondeur des tempos s'ajoutent hors du modèle."
          onChange={(v) => patch({ quality_aplus: v })}
        />

        {/* Sub-settings stay visible when A+ is off — greyed rather than hidden, so
            the cost of switching it on is legible before you commit to it. */}
        <div className="flex items-center gap-1.5 pl-5">
          <span
            className={`text-[10px] ${aplus ? "text-muted-foreground" : "text-muted-foreground/50"}`}
          >
            Seuil d'alerte
          </span>
          <Input
            type="number"
            min={0}
            max={100}
            step="1"
            value={threshold}
            disabled={busy || !aplus}
            onChange={(e) => setThreshold(e.target.value)}
            onBlur={commitThreshold}
            onKeyDown={(e) => {
              if (e.key === "Enter") (e.target as HTMLInputElement).blur();
            }}
            className="h-6 w-14 text-xs tabular-nums"
          />
          <span
            className={`text-[10px] ${aplus ? "text-muted-foreground/70" : "text-muted-foreground/40"}`}
          >
            score final, bonus/pénalités inclus
          </span>
        </div>

        <div className="flex items-center gap-1.5 pl-5">
          <span className={`text-[10px] ${aplus ? "text-muted-foreground" : "text-muted-foreground/50"}`}>
            Budget LLM
          </span>
          <Input
            type="number"
            min={1}
            max={30}
            step="1"
            value={budget}
            disabled={busy || !aplus}
            onChange={(e) => setBudget(e.target.value)}
            onBlur={commitBudget}
            onKeyDown={(e) => { if (e.key === "Enter") (e.target as HTMLInputElement).blur(); }}
            className="h-6 w-14 text-xs tabular-nums"
          />
          <span className={`text-[10px] ${aplus ? "text-muted-foreground/70" : "text-muted-foreground/40"}`}>
            candidats / 5 min
          </span>
        </div>
        <div className="pl-5 text-[10px] leading-snug text-muted-foreground/70">
          Les candidats sont <strong>classés</strong> (attention + RVOL + fraîcheur de la
          news + profondeur des tempos, tous en percentiles du tick), et le budget est
          dépensé du haut de la liste. Ce qui ne rentre pas reste éligible au tick
          suivant — rien n'est perdu, c'est différé derrière mieux classé.
        </div>

        <Row
          indent
          checked={options.news_watch}
          disabled={busy || !aplus}
          label="Veille news continue"
          hint="Surveille les news après la détection et re-note dès qu'un titre tombe. Sans clés Alpaca news, repli Massive toutes les 5 min (source Massive actualisée chaque heure)."
          onChange={(v) => patch({ news_watch: v })}
        />
        <Row
          indent
          checked={options.deep_llm}
          disabled={busy || !aplus}
          label="Analyse approfondie (2ᵉ appel LLM)"
          hint="Désactivée par défaut. Deuxième avis complet (SEC, comptes, dilution), exécuté même si le triage refuse, et sa note REMPLACE le Score Breaking. Jusqu'à 2 min réelles, même en replay accéléré. Son barème n'a pas encore été réécrit pour l'échelle Score Breaking."
          onChange={(v) => patch({ deep_llm: v })}
        />
        <Row
          indent
          checked={options.alert_before_score}
          disabled={busy || !aplus}
          label="Alerter sans attendre le score"
          hint="Publie dès que l'impulsion est confirmée, puis met l'alerte à jour. Plus réactif, plus bruyant."
          onChange={(v) => patch({ alert_before_score: v })}
        />
      </div>

      {/* ── Phone push. Independent of A+ on purpose: with A+ on it rings only for
             what cleared the threshold, with A+ off for every confirmed departure. ── */}
      <div className="space-y-1.5 border-t border-border/40 pt-2">
        <Row
          checked={options.ntfy_alerts}
          disabled={busy}
          label="Alerte téléphone (ntfy)"
          hint={
            aplus
              ? "Priorité maximale : sonnerie + vibreur. Ne sonne que pour une alerte retenue — jamais pour une alerte rétrogradée sous le seuil."
              : "Priorité maximale : sonnerie + vibreur, pour chaque départ confirmé."
          }
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
