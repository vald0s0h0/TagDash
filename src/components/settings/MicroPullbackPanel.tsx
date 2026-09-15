// Micro Pullback's own Settings panel — rendered ONLY under that strategy.
//
// The boolean-cascade detector: six tempos against each ticker's own rolling
// baseline, an n-of-three acceleration rule, then a tape watch that must see the
// prints before anything reaches the screen. No model anywhere in the path.
//
// Dislocation has its own panel next door with a completely different set. They
// were one struct while Dislocation was called "Micro Pullback v2", which is
// exactly the confusion this split removes.
//
// WHY MULTIPLIERS FOR THE GRID. Six tempos × six numbers is a spreadsheet, not a
// panel, and the numbers are not independent: the grid is a deliberate slope —
// longer window, more absolute movement, less relative acceleration. Editing one
// cell destroys that slope while looking like a small change. The three sliders
// move the whole grid and keep its shape, and the resulting 10s row is printed
// underneath so the effect is never abstract.
//
// Every change commits immediately and the engine re-reads the whole struct on its
// next one-second tick, so a toggle takes effect without a restart.

import { useEffect, useState } from "react";

import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import {
  useMicroPullbackOptions,
  useSetMicroPullbackOptions,
} from "@/queries/useScanner";
import { useQuery } from "@tanstack/react-query";
import { api } from "@/lib/tauri";
import type { MicroPullbackOptions } from "@/types";

/** The six detection tempos, in the order the engine evaluates them. Labels match
 *  what the alert text prints, so a badge on screen and a checkbox here are
 *  recognisably the same thing. */
const TEMPOS: { key: keyof MicroPullbackOptions; label: string }[] = [
  { key: "tempo_10s", label: "10s" },
  { key: "tempo_20s", label: "20s" },
  { key: "tempo_40s", label: "40s" },
  { key: "tempo_1m", label: "1m" },
  { key: "tempo_2m", label: "2m" },
  { key: "tempo_5m", label: "5m" },
];

/** The compiled 10-second row, mirrored from `Config::DEFAULT` so the panel can show
 *  what the multipliers actually produce. Kept in sync by hand — the engine's own
 *  test pins the slope, and this only ever renders a hint. */
const BASE_10S = {
  return_pct: 1.0,
  shares: 10_000,
  dollars: 25_000,
  ratios: 8,
};

/** One-off calibration against local flat files (premarket 04:00–09:30 ET),
 *  counting distinct symbols/day that trip at least one active tempo under the
 *  REAL detection logic (`eval_tempo` + `gate3_trips`, same code the engine runs,
 *  universe capped à 400 tickers/jour par volume $ pour rester dans le temps de
 *  calcul). A snapshot, pas une fonctionnalité en direct — voir la note "Repère
 *  historique" qu'elle alimente.
 *
 *  IMPORTANT : ceci mesure les déclenchements de la porte 3 (le calcul de
 *  `eval_tempo`/`gate3_trips`), PAS les alertes réellement confirmées — la montre
 *  de tape (porte finale, qui exige un vrai rythme d'impression) filtre encore un
 *  certain nombre de ces déclenchements en direct. Le nombre réel d'alertes/jour
 *  est donc probablement inférieur à ce tableau, à réglage égal. */
const CALIBRATION = {
  // ×1,0 (grille compilée) : médiane 25/jour. ×3,0 : médiane 6. ×4,0 : médiane 4
  // (P25 2, P75 9). ×5,0 : médiane 3. required=2 (réglage historique) partout.
  summary:
    "×3,5 à ×4,0 sur les trois curseurs ci-dessus ramène la médiane vers 4-5 " +
    "candidats/jour (contre 25/jour à ×1,0, la grille compilée par défaut).",
  sessions: 88,
  period: "fév.–mai 2026",
  ratiosRequired: 2,
};

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
        {hint && <div className="text-[10px] leading-snug text-muted-foreground/70">{hint}</div>}
      </div>
    </div>
  );
}

/** A labelled numeric field that only commits on blur or Enter, so a half-typed
 *  value never reaches the engine. */
function Num({
  label,
  value,
  suffix,
  disabled,
  step,
  min,
  max,
  onCommit,
}: {
  label: string;
  value: string;
  suffix?: string;
  disabled?: boolean;
  step?: string;
  min?: number;
  max?: number;
  onCommit: (raw: string) => void;
}) {
  const [draft, setDraft] = useState(value);
  useEffect(() => setDraft(value), [value]);
  return (
    <div className="flex items-center gap-1.5">
      <span className="w-[8.5rem] shrink-0 text-[10px] text-muted-foreground">{label}</span>
      <Input
        type="number"
        step={step ?? "1"}
        min={min}
        max={max}
        value={draft}
        disabled={disabled}
        onChange={(e) => setDraft(e.target.value)}
        onBlur={() => onCommit(draft)}
        onKeyDown={(e) => {
          if (e.key === "Enter") (e.target as HTMLInputElement).blur();
        }}
        className="h-6 w-20 text-xs tabular-nums"
      />
      {suffix && <span className="text-[10px] text-muted-foreground/70">{suffix}</span>}
    </div>
  );
}

export function MicroPullbackPanel() {
  const { data: options } = useMicroPullbackOptions();
  const setOptions = useSetMicroPullbackOptions();
  // What the live tape physically is. Polled slowly: it changes when a provider
  // connects or when Massive starts supplying `z`, not tick by tick.
  const { data: feed } = useQuery({
    queryKey: ["feed-diagnostics"],
    queryFn: api.getFeedDiagnostics,
    refetchInterval: 5_000,
  });

  if (!options) return null;
  const busy = setOptions.isPending;

  function patch(partial: Partial<MicroPullbackOptions>) {
    setOptions.mutate({ ...options!, ...partial });
  }

  /** Commit a numeric field, ignoring anything unparseable rather than writing NaN. */
  const num = (key: keyof MicroPullbackOptions, transform = (n: number) => n) =>
    (raw: string) => {
      const n = parseFloat(raw);
      if (Number.isFinite(n)) patch({ [key]: transform(n) } as Partial<MicroPullbackOptions>);
    };

  const eff10s = {
    return_pct: BASE_10S.return_pct * options.return_min_scale,
    shares: BASE_10S.shares * options.volume_min_scale,
    dollars: BASE_10S.dollars * options.volume_min_scale,
    ratios: BASE_10S.ratios * options.ratio_min_scale,
  };

  const countsTrades = feed?.tape_counts_trades ?? true;
  const aggregates = feed?.tape_kind === "second_aggregates";

  return (
    <div className="mt-2 space-y-3 rounded border border-border/40 bg-muted/20 p-2.5">
      {/* ── What the tape is. Everything below reads differently depending on it,
             so it is stated first rather than buried in a diagnostics tab. ── */}
      <div className="space-y-1">
        <div className="text-[11px] font-medium">Source de bande</div>
        <div className="text-[10px] leading-snug text-muted-foreground/70">
          {aggregates ? (
            <>
              Agrégats <strong>1 seconde</strong> (Massive).{" "}
              {countsTrades ? (
                <>
                  Le champ <span className="font-mono">z</span> est présent, donc le nombre
                  de trades est reconstruit (<span className="font-mono">n ≈ v / z</span>) et
                  la montre de tape mesure un vrai taux de prints.
                </>
              ) : (
                <>
                  Aucun <span className="font-mono">z</span> reçu pour l'instant : la montre
                  se rabat sur les dollars et la continuité, et le ratio de rythme
                  disparaît de la porte 3.
                </>
              )}
            </>
          ) : (
            <>
              Prints réels ({feed?.feed || "—"}) : un message par transaction, le nombre de
              trades est natif.
            </>
          )}
        </div>
      </div>

      {/* ── Universe ── */}
      <div className="space-y-1.5 border-t border-border/40 pt-2">
        <div className="text-[11px] font-medium">Univers</div>
        <Num
          label="Prix"
          value={String(options.price_min)}
          suffix={`→ ${options.price_max} $`}
          step="0.5"
          disabled={busy}
          onCommit={num("price_min")}
        />
        <Num
          label="Prix maximum"
          value={String(options.price_max)}
          suffix="$"
          step="0.5"
          disabled={busy}
          onCommit={num("price_max")}
        />
        <Num
          label="Float maximum"
          value={String(options.float_max / 1_000_000)}
          suffix="M actions"
          step="1"
          disabled={busy}
          onCommit={num("float_max", (n) => Math.round(n * 1_000_000))}
        />
        <Row
          checked={options.allow_unknown_float}
          disabled={busy}
          label="Surveiller les floats inconnus"
          hint="Un ticker fraîchement listé est souvent celui qui part. L'alerte porte FLOAT_UNKNOWN — la décision reste au trader."
          onChange={(v) => patch({ allow_unknown_float: v })}
        />
        <Row
          checked={options.hot_industry_only}
          disabled={busy}
          label="Industries porteuses uniquement"
          hint="Biotech, spatial, crypto, quantique, cannabis, IA. Une industrie jamais collectée n'est PAS porteuse, donc exclue aussi : c'est le bord tranchant de cette option."
          onChange={(v) => patch({ hot_industry_only: v })}
        />
        <Row
          checked={options.exclude_risk_domicile}
          disabled={busy}
          label="Exclure les juridictions à risque"
          hint="Chine (Hong Kong compris), Îles Caïmans, Îles Vierges britanniques. Une domiciliation jamais collectée n'est jamais signalée, donc jamais exclue : on bannit sur ce qu'on sait."
          onChange={(v) => patch({ exclude_risk_domicile: v })}
        />
      </div>

      {/* ── Tempos ── */}
      <div className="space-y-1.5 border-t border-border/40 pt-2">
        <div className="text-[11px] font-medium">Tempos de détection</div>
        <div className="flex flex-wrap gap-1">
          {TEMPOS.map(({ key, label }) => {
            const on = options[key] as boolean;
            return (
              <button
                key={key}
                type="button"
                disabled={busy}
                onClick={() => patch({ [key]: !on } as Partial<MicroPullbackOptions>)}
                className={`rounded px-2 py-0.5 text-[10px] tabular-nums ring-1 transition-colors ${
                  on
                    ? "bg-foreground/10 text-foreground ring-foreground/30"
                    : "text-muted-foreground/50 ring-border/50 hover:text-foreground/70"
                }`}
              >
                {label}
              </button>
            );
          })}
        </div>
        <div className="text-[10px] leading-snug text-muted-foreground/70">
          Chaque tempo est évalué contre sa propre baseline glissante. Le premier qui
          déclenche arme le ticker ; les autres ne font qu'élargir la profondeur affichée.
        </div>
      </div>

      {/* ── Detection thresholds ── */}
      <div className="space-y-1.5 border-t border-border/40 pt-2">
        <div className="text-[11px] font-medium">Seuils de détection</div>
        <Num
          label="Retour minimum"
          value={String(options.return_min_scale)}
          suffix="× la grille"
          step="0.1"
          min={0.1}
          max={10}
          disabled={busy}
          onCommit={num("return_min_scale")}
        />
        <Num
          label="Volume minimum"
          value={String(options.volume_min_scale)}
          suffix="× la grille"
          step="0.1"
          min={0.1}
          max={10}
          disabled={busy}
          onCommit={num("volume_min_scale")}
        />
        <Num
          label="Ratios minimum"
          value={String(options.ratio_min_scale)}
          suffix="× la grille"
          step="0.1"
          min={0.1}
          max={10}
          disabled={busy}
          onCommit={num("ratio_min_scale")}
        />
        <Num
          label="Ratios exigés"
          value={String(options.ratios_required)}
          suffix="sur 3"
          min={1}
          max={3}
          disabled={busy}
          onCommit={num("ratios_required", (n) => Math.round(n))}
        />
        <div className="rounded bg-background/40 px-2 py-1.5 text-[10px] leading-relaxed text-muted-foreground/80">
          <span className="font-medium text-foreground/80">Effet sur le tempo 10s :</span>{" "}
          retour ≥ <span className="tabular-nums">{eff10s.return_pct.toFixed(2)} %</span> ·
          volume ≥{" "}
          <span className="tabular-nums">
            {Math.round(eff10s.shares).toLocaleString("fr-FR")} actions
          </span>{" "}
          et{" "}
          <span className="tabular-nums">
            {Math.round(eff10s.dollars).toLocaleString("fr-FR")} $
          </span>{" "}
          · ratios ≥ <span className="tabular-nums">×{eff10s.ratios.toFixed(1)}</span>
          <div className="mt-1 text-muted-foreground/60">
            Les tempos plus longs suivent la même pente : plus de mouvement absolu, moins
            d'accélération relative.
          </div>
        </div>
        {options.ratios_required === 3 && !countsTrades && (
          <div className="text-[10px] leading-snug text-amber-500/90">
            Sans compte de trades, seuls deux ratios existent : « 3 sur 3 » veut dire « les
            deux » et n'ajoute rien de plus.
          </div>
        )}
        <div className="rounded bg-background/40 px-2 py-1.5 text-[10px] leading-relaxed text-muted-foreground/80">
          <span className="font-medium text-foreground/80">Repère historique :</span>{" "}
          {CALIBRATION.summary}
          <div className="mt-1 text-muted-foreground/60">
            Mesuré sur {CALIBRATION.sessions} séances premarket ({CALIBRATION.period}),
            ratios exigés = {CALIBRATION.ratiosRequired} (réglage historique), en comptant
            les tickers qui déclenchent au moins un tempo — pas les alertes réellement
            confirmées : la montre de tape (ci-dessous) filtre encore une partie de ces
            déclenchements en direct, donc le compte réel par jour est probablement
            inférieur à ce repère, à réglage égal.
          </div>
          <div className="mt-1 text-muted-foreground/60">
            Point de départ, pas une vérité figée : les cycles de marché changent
            fortement ce nombre d'un mois à l'autre. La ligne "Effet sur le tempo 10s"
            ci-dessus reste la référence en temps réel pour VOTRE réglage actuel.
          </div>
        </div>
      </div>

      {/* ── Tape watch ── */}
      <div className="space-y-1.5 border-t border-border/40 pt-2">
        <div className="text-[11px] font-medium">Montre de tape</div>
        <div className="text-[10px] leading-snug text-muted-foreground/70">
          La détection n'alerte pas, elle <strong>arme</strong>. La montre mesure ensuite la
          bande vivante chaque seconde et ne fait feu que si elle imprime réellement.
        </div>
        <Num
          label="Durée maximale"
          value={String(options.watch_max_secs)}
          suffix="s avant abandon"
          min={5}
          max={600}
          disabled={busy}
          onCommit={num("watch_max_secs", (n) => Math.round(n))}
        />
        <Num
          label="Couche rapide"
          value={String(options.watch_fast_rate_per_sec)}
          suffix="prints/s sur 1 s"
          step="0.5"
          disabled={busy || !countsTrades}
          onCommit={num("watch_fast_rate_per_sec")}
        />
        <Num
          label="Couche lente"
          value={String(options.watch_slow_rate_per_sec)}
          suffix="prints/s sur 5 s"
          step="0.5"
          disabled={busy || !countsTrades}
          onCommit={num("watch_slow_rate_per_sec")}
        />
        <Num
          label="Plancher"
          value={String(options.watch_min_dollar_per_sec)}
          suffix="$/s"
          step="500"
          disabled={busy}
          onCommit={num("watch_min_dollar_per_sec")}
        />
        {!countsTrades && (
          <div className="text-[10px] leading-snug text-muted-foreground/60">
            Les deux taux sont inertes sur cette bande : sans compte de trades, la
            confirmation passe par la continuité et les dollars.
          </div>
        )}
      </div>

      {/* ── Scoring IA ── */}
      <div className="space-y-1.5 border-t border-border/40 pt-2">
        <div className="text-[11px] font-medium">Scoring IA</div>
        <div className="text-[10px] leading-snug text-muted-foreground/70">
          Une passe News rapide (2-4 s) lit la dernière news du ticker et note le
          catalyseur sur 100 — aucun mélange avec la détection tape, qui a déjà décidé
          seule d'armer le ticker. Remplace l'ancien bouton "Analyse IA" en texte libre.
        </div>
        <Row
          checked={options.llm_auto}
          disabled={busy}
          label="Lancer automatiquement le scoring news"
          hint="Coché : chaque nouveau départ est noté sans action, dans la limite du budget ci-dessous — le bouton manuel disparaît (pas de double appel). Décoché : un bouton « Analyser (IA) » apparaît sur l'alerte."
          onChange={(v) => patch({ llm_auto: v })}
        />
        <Num
          label="Budget auto"
          value={String(options.llm_budget_per_5min)}
          suffix="appels / 5 min glissantes"
          min={1}
          max={20}
          disabled={busy || !options.llm_auto}
          onCommit={num("llm_budget_per_5min", (n) => Math.round(n))}
        />
      </div>

      {/* ── Phone ── */}
      <div className="space-y-1.5 border-t border-border/40 pt-2">
        <Row
          checked={options.ntfy_alerts}
          disabled={busy}
          label="Alerte téléphone (ntfy)"
          hint="Priorité maximale : sonnerie + vibreur, à chaque départ confirmé. Un élargissement de profondeur ne re-sonne jamais. Topic et token dans Réglages → API Keys."
          onChange={(v) => patch({ ntfy_alerts: v })}
        />
      </div>
    </div>
  );
}
