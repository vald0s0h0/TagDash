import { Brain, LoaderCircle, Pin, TriangleAlert, X } from "lucide-react";
import { useQuery } from "@tanstack/react-query";
import type { AlertSignal, Position, ScreenerMatch } from "@/types";
import { useUiStore } from "@/stores/uiStore";
import { useLayoutStore } from "@/stores/layoutStore";
import {
  useScreenerPins, useTogglePin, useScreenerRatings, useSetRating,
  usePanicRun, useRunPanicLlm, useHasWebSearch,
} from "@/queries/useScanner";
import { cn } from "@/lib/utils";
import { getCountryMeta } from "@/lib/countries";
import { isHotIndustry, HOT_INDUSTRY_BADGE_CLASS } from "@/lib/industryCategories";
import { api } from "@/lib/tauri";

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

function fmt(v: number | null, decimals = 2): string {
  return v != null ? v.toFixed(decimals) : "—";
}

function fmtGap(v: number | null): string {
  if (v == null) return "—";
  return `${v >= 0 ? "+" : ""}${v.toFixed(1)}%`;
}

function fmtRvol(v: number | null): string {
  if (v == null) return "—";
  return `${v.toFixed(1)}×`;
}

function fmtVol(v: number | null): string {
  if (v == null) return "—";
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
    tempo_depth:    null,
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
 *  the UI. Re-deriving would spend a model call on every name they just discarded.
 *
 *  It takes the matches, not just the symbols, so the button can COUNT what is still
 *  unscored. The watchlist keeps growing after a run (the 09:25 build, every premarket
 *  refresh), and a press then tops up only the missing tickers — the badge is what
 *  tells the user that work is outstanding at all. */
export function PanicScoreButton({ matches }: { matches: ScreenerMatch[] }) {
  const run = usePanicRun().data;
  const launch = useRunPanicLlm();
  const hasWeb = useHasWebSearch().data ?? false;
  // Passed to the run so its catch-up wave skips them: the watchlist is rebuilt every
  // five minutes through the premarket, and the backend has no other way to know which
  // of the newly-arrived names the user already threw out.
  const dismissed = useUiStore((s) => s.dismissedScreener);

  const symbols = matches.map((m) => m.symbol);
  // Unscored = never analysed OR analysed and returned N/A. Both are what a top-up
  // press retries, so both belong in the same count.
  const missing = matches.filter((m) => m.llm_panic_score == null).length;
  const toppingUp = missing > 0 && missing < matches.length;

  const running = run?.running ?? false;
  const nothingToScore = symbols.length === 0;
  const disabled = running || nothingToScore;

  // Phase drives the label so a long run never looks stuck: collection has no
  // per-ticker counter, scoring does, calibration is a single final call.
  const label = !running
    ? toppingUp
      ? `Panic +${missing}`
      : "Panic Score"
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
          toppingUp
            ? `Noter les ${missing} ticker(s) encore sans score (les ${
                matches.length - missing
              } déjà notés sont conservés), puis reclasser la watchlist par panic score.`
            : `Analyser les ${symbols.length} ticker(s) visibles et les classer par panic score décroissant.`,
          run?.error ? `\nDernière tentative : ${run.error}` : "",
          run?.phase === "done" && run.failed.length > 0
            ? `\nDernier run : ${run.failed.length} N/A — ${run.failed.join(", ")}`
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
      onClick={() => launch.mutate({ symbols, dismissed })}
      disabled={disabled}
      title={title}
      className={cn(
        "flex items-center gap-1 rounded px-1.5 py-0.5 text-[10px] font-semibold uppercase tracking-wide transition-colors",
        "bg-violet-900/60 text-violet-200 hover:bg-violet-800/70",
        "disabled:cursor-not-allowed disabled:opacity-40",
        // Tickers still unscored: the ranking is incomplete, so the button says so.
        toppingUp && !running && "bg-amber-900/60 text-amber-100 hover:bg-amber-800/70",
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
          : `Notation ${run.done}/${run.total}${
            run.carried > 0 ? ` (+${run.carried} conservés)` : ""
          }`;
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
            // `done > 0` keeps the bar where it was during the catch-up wave's
            // collection phase, instead of snapping back to 3% mid-run.
            style={{ width: `${run.phase === "scoring" || run.done > 0 ? pct : 3}%` }}
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
          {run.failed.length} ticker(s) en N/A : {run.failed.join(", ")} — classés en bas
          de liste, pas «&nbsp;panique faible&nbsp;». Relancer le bouton les retente.
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
  /** Open internal position on this symbol, if any — colours the symbol badge
   *  by live PnL, exactly like the premarket/open alert cards. */
  position: Position | null;
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

/** A past session's panic score, shown as a watermark to the left of today's
 *  badge: identical shape/colour ramp (`panicTone`) as the live score, just
 *  faded — more so the further back the session is — so the eye reads it as
 *  "same signal, older" rather than a second, competing number. */
function HistoryScoreBadge({
  label,
  score,
  opacity,
}: {
  label: string;
  score: number;
  opacity: string;
}) {
  return (
    <span
      title={`Panic score ${label} : ${score.toFixed(0)}/100 — ${panicBandLabel(score)}.`}
      className={cn(
        "rounded px-1.5 py-0.5 text-xs font-bold tabular-nums",
        panicTone(score),
        opacity,
      )}
    >
      {score.toFixed(0)}
    </span>
  );
}

function ScreenerCard({
  match: m, selected, pinned, rating, position, onOpen, onDismiss, onTogglePin, onRate,
}: CardProps) {
  const gapColor =
    m.gap_pct == null ? "text-muted-foreground"
    : m.gap_pct >= 0 ? "text-emerald-400"
    : "text-red-400";
  const country = getCountryMeta(m.country);
  const hotIndustry = isHotIndustry(m.industry);

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

      {/* Row 1 — the verdict: symbol + panic score (today's, with the previous two
          scored sessions as fading watermarks to its left). `mr-9` reserves room
          for the two absolutely-positioned buttons above. */}
      <div className="mr-9 flex items-center justify-between gap-2">
        {position ? (
          // Open position → the ticker name becomes a P&L-coloured badge
          // (green = winning, red = losing), same treatment as the
          // premarket/open alert cards.
          <span
            title={`Position ${position.side} · PnL ${fmt(position.unrealized_pnl)}`}
            className={cn(
              "rounded px-1.5 py-0.5 text-sm font-bold tabular-nums",
              (position.unrealized_pnl ?? 0) >= 0
                ? "bg-emerald-600/30 text-emerald-200"
                : "bg-red-600/30 text-red-200",
            )}
          >
            {m.symbol}
          </span>
        ) : (
          <span className="text-sm font-semibold tabular-nums">{m.symbol}</span>
        )}
        <div className="flex items-center gap-1">
          {m.llm_panic_score_prev2 != null && (
            <HistoryScoreBadge label="il y a 2 jours" score={m.llm_panic_score_prev2} opacity="opacity-25" />
          )}
          {m.llm_panic_score_prev1 != null && (
            <HistoryScoreBadge label="la veille" score={m.llm_panic_score_prev1} opacity="opacity-50" />
          )}
          {m.llm_panic_score == null && m.llm_eligibility === "n_a" && (
            <span
              title={
                m.llm_one_liner ??
                "Panic score non établi pour ce ticker — classé en bas de liste. Relancer le bouton le retente."
              }
              className="rounded bg-slate-800/70 px-1.5 py-0.5 text-[10px] font-bold uppercase tabular-nums text-slate-400 ring-1 ring-slate-500/30"
            >
              N/A
            </span>
          )}
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
                "rounded px-1.5 py-0.5 text-xs font-bold tabular-nums",
                panicTone(m.llm_panic_score),
              )}
            >
              {m.llm_panic_score.toFixed(0)}
            </span>
          )}
        </div>
      </div>

      {/* Row 2 — origin: country flag (name on hover) + industry, highlighted
          green when it belongs to a historically explosive category. */}
      {(country || m.industry) && (
        <div className="mt-1 flex items-center gap-1.5">
          {country && (
            <span title={`${country.en} / ${country.fr}`} className="shrink-0 text-xs leading-none">
              {country.flag}
            </span>
          )}
          {m.industry && (
            <span
              title={m.industry}
              className={cn(
                "max-w-[140px] truncate rounded px-1.5 py-0.5 text-[9px] font-medium",
                hotIndustry ? HOT_INDUSTRY_BADGE_CLASS : "bg-slate-800/60 text-slate-300",
              )}
            >
              {m.industry}
            </span>
          )}
        </div>
      )}

      {/* Row 3 — the quantitative read: gap · rvol · volume, and the user's own
          rating pushed to the far right of the same line. */}
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
        <span className="ml-auto flex items-center">
          <RatingDots symbol={m.symbol} rating={rating} onRate={onRate} />
        </span>
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
  const { data: positions = [] } = useQuery({
    queryKey: ["internal_positions"],
    queryFn:  () => api.getInternalPositions(),
    refetchInterval: 1000,
  });

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

  /** Dismissing a card advances to the next one below it (falling back to the
   *  previous one if it was last), so working down the list stays a single
   *  stream of X-then-look rather than X-then-hunt-for-the-next-chart. */
  function dismiss(symbol: string) {
    const idx = visible.findIndex((m) => m.symbol === symbol);
    dismissScreener(symbol);
    const next = idx === -1 ? undefined : visible[idx + 1] ?? visible[idx - 1];
    if (next) open(next);
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
            position={positions.find((p) => p.symbol === m.symbol) ?? null}
            onOpen={open}
            onDismiss={dismiss}
            onTogglePin={(symbol) => togglePin.mutate({ symbol, pinned })}
            onRate={(symbol, rating) => setRating.mutate({ symbol, rating })}
          />
        );
      })}
    </div>
  );
}
