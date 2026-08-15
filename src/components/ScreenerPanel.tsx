import { Brain, LoaderCircle, Pin, TriangleAlert, X } from "lucide-react";
import type { AlertSignal, ScreenerMatch } from "@/types";
import { useUiStore } from "@/stores/uiStore";
import { useLayoutStore } from "@/stores/layoutStore";
import {
  useScreenerPins, useTogglePin, useScreenerRatings, useSetRating,
  usePanicRun, useRunPanicLlm, useHasWebSearch,
} from "@/queries/useScanner";
import { cn } from "@/lib/utils";

// ─── Helpers ──────────────────────────────────────────────────────────────────

/** Colour ramp for the Panic Score. HIGH is the attractive end for this strategy: a
 *  high score means the move is NOT justified by fundamentals and is likely to
 *  normalise, which is exactly the mean-reversion setup. So the ramp runs cool
 *  (justified move, leave it alone) → hot (unjustified, tradeable). */
function panicTone(score: number): string {
  if (score >= 81) return "bg-fuchsia-950/70 text-fuchsia-200 ring-1 ring-fuchsia-500/50";
  if (score >= 61) return "bg-rose-950/70 text-rose-200 ring-1 ring-rose-500/40";
  if (score >= 41) return "bg-amber-950/70 text-amber-200 ring-1 ring-amber-500/40";
  if (score >= 21) return "bg-sky-950/70 text-sky-200 ring-1 ring-sky-500/30";
  return "bg-slate-800/70 text-slate-300 ring-1 ring-slate-500/30";
}

/** Human label for the five bands, shown in the tooltip so the number has meaning
 *  without opening the daily pane. */
export function panicBandLabel(score: number): string {
  if (score >= 81) return "panique/euphorie pure";
  if (score >= 61) return "catalyseur faible, flux dominant";
  if (score >= 41) return "ambigu";
  if (score >= 21) return "catalyseur solide, exécution excessive";
  return "repricing justifié";
}

function fmtGap(v: number | null): string {
  if (v == null) return "—";
  return `${v >= 0 ? "+" : ""}${v.toFixed(1)}%`;
}

function fmtRvol(v: number | null): string {
  if (v == null) return "—";
  return `${v.toFixed(1)}×`;
}

function fmtVol(v: number): string {
  if (v >= 1_000_000) return `${(v / 1_000_000).toFixed(1)}M`;
  if (v >= 1_000)     return `${(v / 1_000).toFixed(0)}K`;
  return String(v);
}

/** Build a synthetic AlertSignal so the pre-open zone uses the strategy's card
 *  (panes / indicators / info band) exactly like a real alert placement. Exported
 *  so the controller's ticker-navigation opens screener matches the same way. */
export function matchToAlert(m: ScreenerMatch): AlertSignal {
  return {
    alert_id:       `screener-${m.symbol}-${m.strategy_id}`,
    timestamp:      new Date().toISOString(),
    symbol:         m.symbol,
    strategy_id:    m.strategy_id,
    strategy_name:  m.strategy_name,
    priority:       ((m.priority ?? 4) as 1 | 2 | 3 | 4 | 5),
    session:        "pre_open",
    price:          m.price,
    bid:            null,
    ask:            null,
    spread:         null,
    volume:         m.volume,
    rvol:           m.rvol,
    change_day_pct: m.gap_pct,
    float_shares:   m.float_shares,
    news_today:     false,
    halted:         null,
    latency_ui_ms:  null,
    reason:         `Screener · ${m.strategy_name}`,
    display_timeframe: null,
    side:           null,
  };
}

/** The rows actually rendered in the panel: everything the user has not dismissed
 *  today. Exported so the header's count and the Panic Score button's target list are
 *  the SAME definition — if they drifted, the button would silently analyse a
 *  different set from the one the badge claims. */
export function visibleMatches(
  matches: ScreenerMatch[],
  dismissed: string[],
): ScreenerMatch[] {
  return matches.filter((m) => !dismissed.includes(m.symbol));
}

// ─── Panic Score launch button (screener header) ────────────────────────────────

/** Launches the LLM analysis over the tickers currently VISIBLE in the panel.
 *
 *  The visible list is passed explicitly rather than re-derived server-side: the user
 *  removes technically poor candidates with the X, and those dismissals only exist in
 *  the UI. Re-deriving would spend a model call on every name they just discarded. */
export function PanicScoreButton({ symbols }: { symbols: string[] }) {
  const run = usePanicRun().data;
  const launch = useRunPanicLlm();
  const hasWeb = useHasWebSearch().data ?? false;

  const running = run?.running ?? false;
  const nothingToScore = symbols.length === 0;
  const disabled = running || nothingToScore;

  // Phase drives the label so a long run never looks stuck: collection has no
  // per-ticker counter, scoring does, calibration is a single final call.
  const label = !running
    ? "Panic Score"
    : run?.phase === "collecting"
      ? "Collecte…"
      : run?.phase === "calibrating"
        ? "Calibrage…"
        : `${run?.done ?? 0}/${run?.total ?? symbols.length}`;

  const title = nothingToScore
    ? "Aucun ticker visible à analyser"
    : running
      ? `Analyse en cours — ${label}`
      : [
          `Analyser les ${symbols.length} ticker(s) visibles et les classer par panic score.`,
          run?.error ? `\nDernière tentative : ${run.error}` : "",
          run?.phase === "done" && run.failed.length > 0
            ? `\nDernier run : ${run.failed.length} échec(s) — ${run.failed.join(", ")}`
            : "",
          hasWeb
            ? ""
            : "\nAucune clé Tavily/Brave : la recherche de contexte s'arrêtera aux sources structurées.",
        ]
          .filter(Boolean)
          .join("");

  return (
    <button
      type="button"
      onClick={() => launch.mutate(symbols)}
      disabled={disabled}
      title={title}
      className={cn(
        "flex items-center gap-1 rounded px-1.5 py-0.5 text-[10px] font-semibold uppercase tracking-wide transition-colors",
        "bg-violet-900/60 text-violet-200 hover:bg-violet-800/70",
        "disabled:cursor-not-allowed disabled:opacity-40",
        run?.phase === "error" && !running && "bg-red-950/60 text-red-300",
      )}
    >
      {running ? (
        <LoaderCircle className="h-3 w-3 animate-spin" />
      ) : (
        <Brain className="h-3 w-3" />
      )}
      <span className="tabular-nums">{label}</span>
    </button>
  );
}

/** Visible outcome of the last Panic Score run.
 *
 *  Exists because the first version reported failures only through the button's
 *  `title` tooltip and stderr: a run where every ticker failed was indistinguishable
 *  from the button not working at all. An operation that costs money and a few minutes
 *  has to say out loud what happened to it. */
function PanicRunBanner() {
  const run = usePanicRun().data;
  if (!run) return null;

  if (run.running) {
    const pct =
      run.total > 0 ? Math.round((run.done / run.total) * 100) : 0;
    const phase =
      run.phase === "collecting"
        ? "Collecte des données…"
        : run.phase === "calibrating"
          ? "Calibrage du barème…"
          : `Notation ${run.done}/${run.total}`;
    return (
      <div className="m-2 rounded border border-violet-700/50 bg-violet-950/40 px-2 py-1.5">
        <div className="flex items-center gap-1.5 text-[10px] text-violet-200">
          <LoaderCircle className="h-3 w-3 animate-spin" />
          <span>{phase}</span>
        </div>
        {/* Each call is a thinking-model request (~35–75 s), so the run takes
            minutes on a full watchlist. The bar exists so that looks like progress
            rather than a hang. */}
        <div className="mt-1 h-0.5 w-full overflow-hidden rounded bg-violet-900/60">
          <div
            className="h-full bg-violet-400 transition-all"
            style={{ width: `${run.phase === "scoring" ? pct : 3}%` }}
          />
        </div>
      </div>
    );
  }

  if (run.phase === "error" && run.error) {
    return (
      <div className="m-2 flex items-start gap-1.5 rounded border border-red-700/50 bg-red-950/40 px-2 py-1.5 text-[10px] text-red-200">
        <TriangleAlert className="mt-0.5 h-3 w-3 shrink-0" />
        <span>
          Panic score échoué — {run.error}
          <span className="block text-red-300/60">Détails dans Réglages → Logs.</span>
        </span>
      </div>
    );
  }

  // A partial run is worth reporting: the ranking is usable but incomplete, and the
  // missing tickers would otherwise look like low-panic names rather than unscored.
  if (run.phase === "done" && run.failed.length > 0) {
    return (
      <div className="m-2 flex items-start gap-1.5 rounded border border-amber-700/40 bg-amber-950/30 px-2 py-1.5 text-[10px] text-amber-200">
        <TriangleAlert className="mt-0.5 h-3 w-3 shrink-0" />
        <span>
          {run.failed.length} ticker(s) non noté(s) : {run.failed.join(", ")} — non
          classés, pas «&nbsp;panique faible&nbsp;».
        </span>
      </div>
    );
  }
  return null;
}

// ─── Single screener card ───────────────────────────────────────────────────────

interface CardProps {
  match: ScreenerMatch;
  selected: boolean;
  pinned: boolean;
  /** The user's own 1..3 read of the ticker; 0 = unrated. */
  rating: number;
  onOpen: (m: ScreenerMatch) => void;
  onDismiss: (symbol: string) => void;
  onTogglePin: (symbol: string) => void;
  onRate: (symbol: string, rating: number) => void;
}

/** Three dots the user fills in to remember how much they liked a ticker.
 *
 *  Strictly a memo: nothing on the platform reads it — not the ranking, not a
 *  strategy, not sizing. Click a dot to set that level, double-click anywhere on the
 *  group to clear (the dots go back to a discreet grey). Clicks are stopped from
 *  reaching the card so rating never opens the chart by accident. */
function RatingDots({
  symbol,
  rating,
  onRate,
}: {
  symbol: string;
  rating: number;
  onRate: (symbol: string, rating: number) => void;
}) {
  return (
    <div
      title={
        rating > 0
          ? `Ta note : ${rating}/3 — double-clic pour retirer`
          : "Noter le potentiel (1 à 3) — mémo personnel, sans effet sur la plateforme"
      }
      onClick={(e) => e.stopPropagation()}
      onDoubleClick={(e) => { e.stopPropagation(); onRate(symbol, 0); }}
      className="flex items-center gap-[3px]"
    >
      {[1, 2, 3].map((n) => (
        <button
          key={n}
          aria-label={`Note ${n} sur 3`}
          onClick={(e) => { e.stopPropagation(); onRate(symbol, n); }}
          className={cn(
            "h-2 w-2 rounded-full border transition-colors",
            n <= rating
              ? "border-amber-400 bg-amber-400"
              : "border-muted-foreground/25 bg-transparent hover:border-amber-400/60",
          )}
        />
      ))}
    </div>
  );
}

function ScreenerCard({
  match: m, selected, pinned, rating, onOpen, onDismiss, onTogglePin, onRate,
}: CardProps) {
  const gapColor =
    m.gap_pct == null ? "text-muted-foreground"
    : m.gap_pct >= 0 ? "text-emerald-400"
    : "text-red-400";

  return (
    <div
      className={cn(
        "group relative m-2 cursor-pointer rounded-md border border-border bg-card px-3 py-2 transition-colors hover:border-blue-700/60 hover:bg-accent/30",
        selected && "border-blue-600 bg-accent/40",
      )}
      onClick={() => onOpen(m)}
    >
      {/* Pin + dismiss. A pin stays fully visible so the user can see at a glance
          what is pinned; the dismiss X only appears on hover, as before. */}
      <div className="absolute right-1.5 top-1.5 flex gap-0.5">
        <button
          title={pinned ? "Retirer le pin (le ticker cesse de suivre les jours suivants)" : "Épingler — garder ce ticker les jours suivants"}
          onClick={(e) => { e.stopPropagation(); onTogglePin(m.symbol); }}
          className={cn(
            "rounded p-0.5 transition-opacity hover:bg-accent",
            pinned
              ? "text-amber-400 opacity-100 hover:text-amber-300"
              : "text-muted-foreground/40 opacity-0 hover:text-amber-400 group-hover:opacity-100",
          )}
        >
          <Pin className={cn("h-3.5 w-3.5", pinned && "fill-current")} />
        </button>
        <button
          title="Retirer de la liste"
          onClick={(e) => { e.stopPropagation(); onDismiss(m.symbol); }}
          className="rounded p-0.5 text-muted-foreground/40 opacity-0 transition-opacity hover:bg-accent hover:text-red-400 group-hover:opacity-100"
        >
          <X className="h-3.5 w-3.5" />
        </button>
      </div>

      {/* Symbol + price. `mr-9` reserves room for the two absolutely-positioned
          buttons above. */}
      <div className="mr-9 flex items-baseline gap-2">
        <span className="text-sm font-semibold tabular-nums">{m.symbol}</span>
        {m.price != null && (
          <span className="text-xs tabular-nums text-foreground/80">${m.price.toFixed(2)}</span>
        )}
        {/* Raw contextual reversibility. Final order also uses extension, confidence
            and the structured eligibility/veto state shown below. */}
        {m.llm_panic_score != null && (
          <span
            title={[
              `Panic score ${m.llm_panic_score.toFixed(0)}/100 — ${panicBandLabel(m.llm_panic_score)}.`,
              "Haut = mouvement non justifié, forte probabilité de normalisation (bon candidat mean-reversion).",
              m.llm_reversibility ? `Réversibilité : ${m.llm_reversibility}.` : "",
              m.llm_one_liner ? `\n${m.llm_one_liner}` : "",
            ]
              .filter(Boolean)
              .join(" ")}
            className={cn(
              "ml-auto rounded px-1.5 py-0.5 text-xs font-bold tabular-nums",
              panicTone(m.llm_panic_score),
            )}
          >
            {m.llm_panic_score.toFixed(0)}
          </span>
        )}
      </div>

      {/* The one-sentence read — the whole point of the analysis, so it is on the
          card and not only in a tooltip. Clamped to two lines to keep the list
          scannable; the full text lives in the daily pane's report. */}
      {m.llm_one_liner != null && (
        <p className="mr-9 mt-1 line-clamp-2 text-[10px] leading-snug text-muted-foreground">
          {m.llm_one_liner}
        </p>
      )}

      {(m.llm_trade_direction != null || m.llm_eligibility != null || m.llm_timing_state != null) && (
        <div className="mt-1 flex flex-wrap gap-1 text-[9px] font-semibold uppercase">
          {m.llm_trade_direction != null && (
            <span className="rounded bg-slate-800 px-1 py-0.5 text-slate-200">
              {m.llm_trade_direction}
            </span>
          )}
          {m.llm_eligibility != null && (
            <span
              title={m.llm_veto_reason ?? undefined}
              className={cn(
                "rounded px-1 py-0.5",
                m.llm_eligibility === "veto"
                  ? "bg-red-950 text-red-300"
                  : m.llm_eligibility === "review"
                    ? "bg-amber-950 text-amber-300"
                    : "bg-emerald-950 text-emerald-300",
              )}
            >
              {m.llm_eligibility === "review" ? "à revoir" : m.llm_eligibility}
            </span>
          )}
          {m.llm_timing_state != null && m.llm_timing_state !== "unknown" && (
            <span
              title="Phase descriptive uniquement — aucun signal automatique d’entrée"
              className="rounded bg-slate-900 px-1 py-0.5 text-slate-400"
            >
              {m.llm_timing_state}
            </span>
          )}
          {m.llm_confidence_score != null && (
            <span className="rounded bg-slate-900 px-1 py-0.5 text-slate-400">
              conf {m.llm_confidence_score.toFixed(0)}
            </span>
          )}
          {m.llm_ranking_score != null && (
            <span className="rounded bg-slate-900 px-1 py-0.5 text-slate-400">
              rang {m.llm_ranking_score.toFixed(0)}
            </span>
          )}
        </div>
      )}

      {/* Quantitative panic reading: σ = how exceptional the move is for THIS ticker,
          × = by how much its ATR expanded, then the window in days. Demoted to a
          secondary chip once the LLM score exists. */}
      {m.score_label != null && (
        <div className="mt-1 flex">
          <span
            title="σ = ampleur du choc rapportée à la volatilité habituelle du titre · × = expansion de l'ATR · nj = fenêtre retenue"
            className={cn(
              "rounded px-1.5 py-0.5 text-[9px] font-semibold tabular-nums",
              m.score == null
                ? "bg-amber-900/50 text-amber-200"
                : "bg-violet-900/40 text-violet-300/90",
            )}
          >
            {m.score_label}
          </span>
        </div>
      )}

      {/* Gap · RVOL · Volume */}
      <div className="mt-1 flex flex-wrap items-center gap-x-3 gap-y-0.5 text-[11px] tabular-nums">
        <span className="flex items-center gap-1">
          <span className="text-[8px] uppercase tracking-wide text-muted-foreground/50">Gap</span>
          <span className={gapColor}>{fmtGap(m.gap_pct)}</span>
        </span>
        <span className="flex items-center gap-1">
          <span className="text-[8px] uppercase tracking-wide text-muted-foreground/50">RVol</span>
          <span className="text-foreground/80">{fmtRvol(m.rvol)}</span>
        </span>
        <span className="flex items-center gap-1">
          <span className="text-[8px] uppercase tracking-wide text-muted-foreground/50">Vol</span>
          <span className="text-muted-foreground">{fmtVol(m.volume)}</span>
        </span>
      </div>

      {/* Strategy chip, and the user's own rating in the bottom-right corner. */}
      <div className="mt-1.5 flex items-center justify-between gap-2">
        <span className="rounded bg-blue-900/40 px-1.5 py-0.5 text-[9px] font-medium text-blue-300">
          {m.strategy_name}
        </span>
        <RatingDots symbol={m.symbol} rating={rating} onRate={onRate} />
      </div>
    </div>
  );
}

// ─── Screener panel (pre-open sidebar) ──────────────────────────────────────────

interface Props {
  matches: ScreenerMatch[];
}

export function ScreenerPanel({ matches }: Props) {
  const selectedTicker    = useUiStore((s) => s.selectedTicker);
  const setSelectedTicker = useUiStore((s) => s.setSelectedTicker);
  const dismissed         = useUiStore((s) => s.dismissedScreener);
  const dismissScreener   = useUiStore((s) => s.dismissScreener);
  const openInActiveZone  = useLayoutStore((s) => s.openInActiveZone);
  const pins              = useScreenerPins().data ?? [];
  const togglePin         = useTogglePin();
  const ratings           = useScreenerRatings().data ?? {};
  const setRating         = useSetRating();

  // Dismissal wins over a pin for the current day: the X hides the row until
  // tomorrow, the pin survives it and brings the ticker back.
  //
  // Pins float to the top; everything else keeps the SERVER's order, which is the
  // Panic Score ranking once an analysis has run (see the scanner's sort). Array
  // sort is stable, so this reorders only across the pinned/unpinned boundary.
  const visible = visibleMatches(matches, dismissed)
    .slice()
    .sort((a, b) => Number(pins.includes(b.symbol)) - Number(pins.includes(a.symbol)));

  function open(m: ScreenerMatch) {
    setSelectedTicker(m.symbol);
    openInActiveZone(matchToAlert(m));
  }

  if (visible.length === 0) {
    return (
      <p className="p-4 text-xs text-muted-foreground">
        Screener pre-open actif — aucun ticker ne remplit les critères pour l'instant.
      </p>
    );
  }

  return (
    <div>
      <PanicRunBanner />
      {visible.map((m) => {
        const pinned = pins.includes(m.symbol);
        return (
          <ScreenerCard
            key={`${m.symbol}-${m.strategy_id}`}
            match={m}
            selected={selectedTicker === m.symbol}
            pinned={pinned}
            rating={ratings[m.symbol] ?? 0}
            onOpen={open}
            onDismiss={dismissScreener}
            onTogglePin={(symbol) => togglePin.mutate({ symbol, pinned })}
            onRate={(symbol, rating) => setRating.mutate({ symbol, rating })}
          />
        );
      })}
    </div>
  );
}
