// Presentational helpers extracted from ChartZone: priority styling, info-band
// field formatting/rendering, and the small toolbar widgets. All are pure /
// self-contained (no shared refs, no ChartZone state), so they live here to keep
// ChartZone focused on orchestration.

import { useEffect, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import type { RefObject } from "react";
import { Ban, ChevronRight, LoaderCircle } from "lucide-react";
import type {
  AlertEnrichment, CardInfo, CardNews, HodDriveOverlay, InfoField, StrategyCard,
  MicroQualityLive, MicroQualityRow,
  PanicDataQuality, PanicLlmScore, TickerLiveState, Timeframe, ZoneAssignment,
} from "@/types";
import { api } from "@/lib/tauri";
import { cn } from "@/lib/utils";
import { useShortability } from "@/queries/useShortability";
import { getCountryMeta } from "@/lib/countries";
import { isHotIndustry, HOT_INDUSTRY_BADGE_CLASS } from "@/lib/industryCategories";

// ─── Priority colours ─────────────────────────────────────────────────────────

export const PRIORITY_STYLES: Record<number, { badge: string; accent: string }> = {
  1: { badge: "bg-zinc-700 text-zinc-300",               accent: "" },
  2: { badge: "bg-blue-900/60 text-blue-300",             accent: "" },
  3: { badge: "bg-amber-900/60 text-amber-300",           accent: "" },
  4: { badge: "bg-orange-900/60 text-orange-300",         accent: "" },
  5: { badge: "bg-red-900/70 text-red-300 animate-pulse", accent: "" },
};

export const TIMEFRAMES: Timeframe[] = ["5s", "10s", "1m", "2m", "5m", "15m", "daily"];

// ─── Info-band field resolution ───────────────────────────────────────────────

export function fmtCompact(v: number): string {
  if (v >= 1_000_000) return `${(v / 1_000_000).toFixed(1)}M`;
  if (v >= 1_000)     return `${(v / 1_000).toFixed(0)}K`;
  return String(Math.round(v));
}

/** Money with a $ prefix and B/M/K scaling (market cap). */
export function fmtMoney(v: number): string {
  if (v >= 1_000_000_000) return `$${(v / 1_000_000_000).toFixed(1)}B`;
  if (v >= 1_000_000)     return `$${(v / 1_000_000).toFixed(0)}M`;
  if (v >= 1_000)         return `$${(v / 1_000).toFixed(0)}K`;
  return `$${Math.round(v)}`;
}

/** Classification → human label for the strategy overlay (mirrors EnrichmentBand). */
const CLASS_LABEL: Record<string, string> = {
  momo_former: "Momo former",
  pump_dump:   "Pump & dump",
};

/** Resolve a strategy-card info-field key to a formatted value, or null when not
 *  (yet) available. `extras` carries per-symbol DB data (score / market cap /
 *  float / industry / country, from get_card_info); `enr` carries the async
 *  enrichment payload (float / country / split / profile); everything else comes
 *  from the live snapshot (TickerLiveState). Keys that resolve to null show "—"
 *  (alert source) or a spinner (llm/enrichment source). */
export function resolveFieldValue(
  key: string,
  live: TickerLiveState | null,
  extras: CardInfo | null,
  enr: AlertEnrichment | null = null,
): string | null {
  // Enrichment-sourced fields (filled progressively by the pipeline).
  switch (key) {
    case "days_since_split":
      return enr?.days_since_split != null && enr.split_label
        ? `${enr.days_since_split}j · ${enr.split_label}` : null;
    case "classification":
      return enr?.classification ? (CLASS_LABEL[enr.classification] ?? enr.classification) : null;
  }
  // Extras-sourced fields (mean-reversion score, market cap, float, meta).
  switch (key) {
    case "mr_score": {
      if (extras?.mr_score == null) return null;
      // List tag + metric value + direction arrow, e.g. "BB 4.2 ▲" / "MA 3.1 ▼".
      const arrow = extras.mr_direction == null || extras.mr_direction === 0
        ? "" : extras.mr_direction > 0 ? "▲" : "▼";
      return [
        extras.mr_score_kind ?? "",
        extras.mr_score.toFixed(1),
        arrow,
      ].filter(Boolean).join(" ");
    }
    case "market_cap":
      return extras?.market_cap != null ? fmtMoney(extras.market_cap) : null;
    case "float_shares": {
      const fl = enr?.float_shares ?? extras?.float_shares ?? null;
      return fl != null ? fmtCompact(fl) : null;
    }
    case "industry":
      return enr?.industry ?? extras?.industry ?? null;
    case "country":
      return enr?.country ?? extras?.country ?? null;
  }
  if (!live) return null;
  switch (key) {
    case "change_day_pct": return live.change_day_pct != null
      ? `${live.change_day_pct >= 0 ? "+" : ""}${live.change_day_pct.toFixed(1)}%` : null;
    case "volume":         return live.volume_day != null ? fmtCompact(live.volume_day) : null;
    case "spread":         return live.spread != null ? `$${live.spread.toFixed(2)}` : null;
    case "vwap":           return live.vwap != null ? `$${live.vwap.toFixed(2)}` : null;
    case "price":          return live.last_price != null ? `$${live.last_price.toFixed(2)}` : null;
    case "bid":            return live.bid != null ? `$${live.bid.toFixed(2)}` : null;
    case "ask":            return live.ask != null ? `$${live.ask.toFixed(2)}` : null;
    default:               return null; // rvol / float_shares / llm_* → pending / —
  }
}

// ─── Info-band field chip ─────────────────────────────────────────────────────

export function FieldChip({ field, value }: { field: InfoField; value: string | null }) {
  // alert-source fields with no value show "—"; llm / enrichment fields show a
  // spinner to signal "coming" until their API populates the value.
  const pending = value == null && field.source !== "alert";
  return (
    <span className="flex items-center gap-0.5">
      <span className="text-[8px] uppercase tracking-wide text-muted-foreground/45">
        {field.label}
      </span>
      {value != null ? (
        <span className="text-[10px] tabular-nums text-foreground/80">{value}</span>
      ) : pending ? (
        <LoaderCircle className="h-2.5 w-2.5 animate-spin text-blue-400/70" />
      ) : (
        <span className="text-[10px] text-muted-foreground/30">—</span>
      )}
    </span>
  );
}

// ─── Strategy badge colours ───────────────────────────────────────────────────

/** Per-strategy badge colour for the common info bar. Unknown ids fall back to a
 *  neutral zinc badge — the label still reads, just without a dedicated hue. */
const STRATEGY_BADGE: Record<string, string> = {
  micro_pullback:       "bg-amber-900/60 text-amber-300",
  panic_mean_reversion: "bg-red-900/60 text-red-300",
  perfect_pullback:     "bg-emerald-900/60 text-emerald-300",
  hod_drive:            "bg-teal-900/60 text-teal-300",
  backside_parabolic:   "bg-sky-900/60 text-sky-300",
  premarket_gapper:     "bg-violet-900/60 text-violet-300",
  premarket_frd_runner: "bg-violet-900/60 text-violet-300",
  low_float_runner:     "bg-fuchsia-900/60 text-fuchsia-300",
  opening_interest:     "bg-cyan-900/60 text-cyan-300",
};

export function StrategyBadge({
  strategyId,
  name,
}: {
  strategyId: string | null;
  name: string | null;
}) {
  if (!name) return null;
  const cls = (strategyId && STRATEGY_BADGE[strategyId]) || "bg-zinc-700/70 text-zinc-300";
  return (
    <span className={cn("shrink-0 rounded px-1.5 py-0.5 text-[10px] font-semibold", cls)}>
      {name}
    </span>
  );
}

// ─── Common chart info bar (identical across strategies) ──────────────────────

/** Compact "label + value" cell for the common info bar. */
function BarChip({ label, children }: { label?: string; children: React.ReactNode }) {
  return (
    <span className="flex shrink-0 items-center gap-1">
      {label && (
        <span className="text-[8px] uppercase tracking-wide text-muted-foreground/45">{label}</span>
      )}
      <span className="flex items-center gap-0.5 text-[11px] tabular-nums text-foreground/85">
        {children}
      </span>
    </span>
  );
}

/** The shared info bar shown above every chart, identical for all strategies and
 *  merged onto the same line as the ticker/toolbar header: strategy badge ·
 *  country · industry · float · gap · rvol · volume since 04:00 ET. Strategy-
 *  specific fields live in the on-chart overlay (StrategyInfoOverlay), not here.
 *  `bandRef` is forwarded from ChartZone so its screenshot capture can rasterize
 *  just this segment (not the ticker/toolbar around it). */
export function ChartInfoBar({
  bandRef,
  zone,
  cardInfo,
  enrichment,
  live,
}: {
  bandRef: RefObject<HTMLDivElement>;
  zone: ZoneAssignment;
  cardInfo: CardInfo | null;
  enrichment: AlertEnrichment | null;
  live: TickerLiveState | null;
}) {
  // Broker-labelled by design: ETB/HTB criteria are venue-specific and must never
  // look like a universal property of the ticker.
  const shortability = useShortability(zone.symbol);
  const shortabilityBroker = shortability?.broker === "tagdash" ? "TagDash" : "Alpaca";

  // Enrichment (per-alert pipeline) wins over the polled card-info extras once it
  // lands — same precedence as the on-chart overlay's resolveFieldValue.
  const country = getCountryMeta(enrichment?.country ?? cardInfo?.country ?? null);
  const industry = enrichment?.industry ?? cardInfo?.industry ?? null;
  const floatShares = enrichment?.float_shares ?? cardInfo?.float_shares ?? null;

  const gapPct = live?.change_day_pct ?? null;
  const gapCls = gapPct == null ? "" : gapPct >= 0 ? "text-emerald-400" : "text-red-400";

  const rvol = cardInfo?.rvol ?? null;
  const volSince4am = cardInfo?.volume_since_4am ?? null;

  return (
    <div ref={bandRef} className="flex shrink-0 items-center gap-x-3">
      <StrategyBadge strategyId={zone.strategy_id} name={zone.strategy_name} />

      {/* Country of origin — flag only; the name (EN/FR) is a hover tooltip. */}
      {country && (
        <span title={`${country.en} / ${country.fr}`} className="shrink-0 text-sm leading-none">
          {country.flag}
        </span>
      )}

      <BarChip>
        {industry ? (
          isHotIndustry(industry) ? (
            <span
              title={industry}
              className={cn("max-w-[150px] truncate rounded px-1 py-0.5", HOT_INDUSTRY_BADGE_CLASS)}
            >
              {industry}
            </span>
          ) : (
            <span className="max-w-[150px] truncate" title={industry}>{industry}</span>
          )
        ) : (
          <span className="text-muted-foreground/30">—</span>
        )}
      </BarChip>

      <BarChip label="Float">
        {floatShares != null
          ? fmtCompact(floatShares)
          : <span className="text-muted-foreground/30">—</span>}
      </BarChip>

      {/* Premarket gap vs previous close, still updating live post-open. */}
      <BarChip label="Gap">
        {gapPct != null
          ? <span className={gapCls}>{gapPct >= 0 ? "+" : ""}{gapPct.toFixed(1)}%</span>
          : <span className="text-muted-foreground/30">—</span>}
      </BarChip>

      {/* This legacy figure is cumulative day volume / ADV20, not a tempo- or
          time-of-day RVOL. Name it honestly; Micro Pullback's real tempo RVOL is
          shown in its own ranking panel and alert reason. */}
      <BarChip label="Vol/ADV">
        {rvol != null
          ? `${rvol.toFixed(1)}×`
          : <span className="text-muted-foreground/30">—</span>}
      </BarChip>

      {/* Cumulative volume since 04:00 ET (premarket included). */}
      <BarChip label="Vol">
        {volSince4am != null
          ? fmtCompact(volSince4am)
          : <span className="text-muted-foreground/30">—</span>}
      </BarChip>

      {shortability && (
        <span
          title={
            shortability.from_cache
              ? `Statut ${shortabilityBroker} en cache — disponibilité borrow non confirmée en temps réel`
              : shortability.borrow_status === "easy_to_borrow"
                ? `${shortabilityBroker} · Easy-to-Borrow — aucun locate requis`
                : shortability.borrow_status === "hard_to_borrow"
                  ? `${shortabilityBroker} · Hard-to-Borrow — locate requis avant le short`
                  : shortability.borrow_status === "unavailable"
                    ? `${shortabilityBroker} ne permet pas de shorter ce titre`
                    : `Statut borrow ${shortabilityBroker} inconnu`
          }
          className={cn(
            "flex items-center gap-1 rounded border px-1.5 py-0.5 text-[10px] font-medium",
            shortability.borrow_status === "easy_to_borrow"
              ? "border-emerald-700/50 bg-emerald-950/30 text-emerald-300"
              : shortability.borrow_status === "hard_to_borrow"
                ? "border-amber-600/50 bg-amber-950/35 text-amber-300"
                : shortability.borrow_status === "unavailable"
                  ? "border-red-600/50 bg-red-950/40 text-red-300"
                  : "border-zinc-600/50 bg-zinc-900/50 text-zinc-400",
          )}
        >
          {shortability.borrow_status === "unavailable" && <Ban className="h-3 w-3" />}
          {shortabilityBroker} · {shortability.borrow_status === "easy_to_borrow" ? "ETB"
            : shortability.borrow_status === "hard_to_borrow" ? "HTB"
            : shortability.borrow_status === "unavailable" ? "non shortable" : "borrow ?"}
          {shortability.from_cache && " · cache"}
        </span>
      )}
    </div>
  );
}

// ─── On-chart strategy info overlay (top-left of the left pane) ───────────────

/** Strategy-specific fields drawn directly on the chart, top-left of the left
 *  pane. Transparent background (chart shows through), ~1.5× text, one cell per
 *  field. Renders only the strategy's own info_fields — the common metrics live
 *  in ChartInfoBar above the chart. A strategy-specific LLM button + verdict zone
 *  will slot in here once strategies declare one. */
export function StrategyInfoOverlay({
  card,
  live,
  cardInfo,
  enrichment,
}: {
  card: StrategyCard | null;
  live: TickerLiveState | null;
  cardInfo: CardInfo | null;
  enrichment: AlertEnrichment | null;
}) {
  const fields = card?.info_fields ?? [];
  if (fields.length === 0) return null;
  return (
    <div
      data-capture-overlay
      className="pointer-events-none absolute left-1.5 top-1.5 z-10 flex max-w-[70%] flex-wrap gap-1.5"
    >
      {fields.map((f) => {
        const value = resolveFieldValue(f.key, live, cardInfo, enrichment);
        const pending = value == null && f.source !== "alert";
        return (
          <div
            key={f.key}
            data-capture-cell
            className="flex flex-col rounded bg-black/35 px-1.5 py-0.5 backdrop-blur-[1px]"
          >
            <span className="text-[9px] uppercase leading-none tracking-wide text-muted-foreground/60">
              {f.label}
            </span>
            {value != null ? (
              <span className="text-[15px] font-semibold leading-tight tabular-nums text-foreground/90">
                {value}
              </span>
            ) : pending ? (
              <LoaderCircle className="mt-0.5 h-3.5 w-3.5 animate-spin text-blue-400/70" />
            ) : (
              <span className="text-[15px] font-semibold leading-tight text-muted-foreground/30">—</span>
            )}
          </div>
        );
      })}
    </div>
  );
}

// ─── Panic Score report (daily pane, Panic Mean Reversion) ────────────────────

/** Same five-band ramp as the screener card, kept here so the report and the card
 *  read as one system. High = unjustified move = attractive mean-reversion setup. */
function reportTone(score: number): { ring: string; text: string } {
  if (score >= 81) return { ring: "ring-fuchsia-500/60", text: "text-fuchsia-200" };
  if (score >= 61) return { ring: "ring-rose-500/50", text: "text-rose-200" };
  if (score >= 41) return { ring: "ring-amber-500/50", text: "text-amber-200" };
  if (score >= 21) return { ring: "ring-sky-500/40", text: "text-sky-200" };
  return { ring: "ring-slate-500/40", text: "text-slate-300" };
}

function bandLabel(score: number): string {
  if (score >= 81) return "panique / euphorie pure";
  if (score >= 61) return "catalyseur faible, flux dominant";
  if (score >= 41) return "ambigu";
  if (score >= 21) return "catalyseur solide, exécution excessive";
  return "repricing justifié";
}

/** The LLM's report for the displayed ticker, drawn on the DAILY pane.
 *
 *  Information hierarchy is deliberate and top-down, because the trader reads this in
 *  the seconds before deciding:
 *    1. the panic score, biggest thing on the pane — it is the verdict;
 *    2. the one-sentence explanation — why it moves and how reversible that looks;
 *    3. catalyst strength / confidence / normalisation probability — the qualifiers;
 *    4. the factual drivers as chips — the evidence behind the number;
 *    5. provenance in small print — feed, replay flag, web use, rubric version.
 *
 *  Point 5 is not decoration: a score built on IEX-only volume with no news source
 *  deserves less weight than one built on SIP plus a filing, and the only honest way
 *  to convey that is to show it. */
export function PanicReportOverlay({ score }: { score: PanicLlmScore | null }) {
  const [expanded, setExpanded] = useState(false);
  if (!score) return null;

  const quality: PanicDataQuality = (() => {
    if (!score.data_quality) return {};
    try {
      return JSON.parse(score.data_quality) as PanicDataQuality;
    } catch {
      return {};
    }
  })();

  // Analysed, but no score could be established. Reported rather than hidden: an
  // empty pane is indistinguishable from "never analysed", and the difference decides
  // whether the trader should look for the context by hand.
  if (score.panic_score == null) {
    return (
      <div
        data-capture-overlay
        className="pointer-events-none absolute left-1.5 top-1.5 z-10 flex max-w-[47%] flex-col gap-1 rounded bg-black/55 px-2 py-1.5 backdrop-blur-[2px]"
      >
        <div className="flex items-baseline gap-2">
          <span className="rounded px-2 py-0.5 text-[22px] font-bold leading-none text-slate-300 ring-1 ring-slate-500/40">
            N/A
          </span>
          <span className="text-[11px] font-medium leading-tight text-slate-300">
            non établi — classé en bas
          </span>
        </div>
        {score.one_liner && (
          <p className="text-[11px] leading-snug text-foreground/90">
            {score.one_liner}
          </p>
        )}
        <span className="text-[8px] leading-tight text-muted-foreground/50">
          {[
            quality.feed ? `feed ${quality.feed.toUpperCase()}` : null,
            score.simulated ? "replay (sans web)" : null,
            "relancer le bouton retente ce ticker",
          ]
            .filter(Boolean)
            .join(" · ")}
        </span>
      </div>
    );
  }

  const s = score.panic_score;
  const tone = reportTone(s);

  const provenance = [
    quality.feed ? `feed ${quality.feed.toUpperCase()}` : null,
    score.simulated ? "replay (sans web)" : null,
    score.web_used ? `web ${score.web_provider ?? ""}`.trim() : null,
    quality.missing?.length ? `${quality.missing.length} champ(s) absent(s)` : null,
    quality.lookahead?.length ? "instantanés actuels" : null,
    score.rubric_version ?? null,
  ].filter(Boolean) as string[];

  const hasComplementary =
    score.trade_direction != null || score.eligibility != null || score.timing_state != null
    || score.key_drivers.length > 0 || score.forced_flow != null || score.euphoria != null
    || provenance.length > 0 || score.adjustment_reason != null;

  return (
    <div
      data-capture-overlay
      className="pointer-events-none absolute left-1.5 top-1.5 z-10 flex max-w-[47%] flex-col gap-1 rounded bg-black/55 px-2 py-1.5 backdrop-blur-[2px]"
    >
      {/* ── Essentials ── */}

      {/* 1 — the verdict */}
      <div className="flex items-baseline gap-2">
        <span
          className={cn(
            "rounded px-2 py-0.5 text-[26px] font-bold leading-none tabular-nums ring-1",
            tone.ring,
            tone.text,
          )}
        >
          {s.toFixed(0)}
        </span>
        <span className={cn("text-[11px] font-medium leading-tight", tone.text)}>
          {bandLabel(s)}
        </span>
      </div>

      {/* 2 — why */}
      {score.one_liner && (
        <p className="text-[11px] leading-snug text-foreground/90">
          {score.one_liner}
        </p>
      )}

      {/* 3 — the qualifiers */}
      <div className="flex flex-wrap items-center gap-x-2.5 gap-y-0.5 text-[10px] tabular-nums">
        <ReportStat label="Catalyseur" value={score.catalyst_strength} suffix="/100" />
        <ReportStat label="Confiance" value={score.confidence_score} suffix="/100" />
        <ReportStat label="Normalisation" value={score.normalization_prob} suffix="%" />
        <ReportStat label="Classement" value={score.ranking_score} suffix="/100" />
      </div>

      {/* ── Toggle → complementary info ── */}
      {hasComplementary && (
        <button
          type="button"
          onClick={() => setExpanded((v) => !v)}
          className="pointer-events-auto flex items-center gap-1 self-start text-[9px] uppercase tracking-wide text-muted-foreground/60 hover:text-foreground/80"
        >
          <ChevronRight className={cn("h-2.5 w-2.5 transition-transform", expanded && "rotate-90")} />
          {expanded ? "moins" : "plus"}
        </button>
      )}

      {expanded && (
        <div className="flex flex-col gap-1 border-t border-white/10 pt-1">
          {score.catalyst_label && (
            <span className="text-[10px] text-foreground/70">{score.catalyst_label}</span>
          )}

          {(score.trade_direction || score.eligibility || score.timing_state) && (
            <div className="flex flex-wrap gap-1 text-[9px] font-semibold uppercase">
              {score.trade_direction && <span>{score.trade_direction}</span>}
              {score.eligibility && <span>· {score.eligibility}</span>}
              {score.timing_state && <span>· phase {score.timing_state}</span>}
              {score.fundamental_veto && score.veto_reason && (
                <span className="normal-case text-red-300">· veto : {score.veto_reason}</span>
              )}
            </div>
          )}

          {/* the evidence */}
          {score.key_drivers.length > 0 && (
            <div className="flex flex-wrap gap-1">
              {score.key_drivers.map((d) => (
                <span
                  key={d}
                  className="rounded bg-white/10 px-1.5 py-0.5 text-[9px] leading-none text-foreground/80"
                >
                  {d}
                </span>
              ))}
            </div>
          )}

          {/* Forced flow / euphoria — the strategy's two behavioural reads. */}
          {(score.forced_flow || score.euphoria) && (
            <div className="flex flex-col gap-0.5 text-[10px] leading-snug text-muted-foreground">
              {score.forced_flow && <span>⇉ {score.forced_flow}</span>}
              {score.euphoria && <span>◎ {score.euphoria}</span>}
            </div>
          )}

          {/* provenance, plus any calibration note so an adjusted score is never
              silently different from what the model first said. */}
          {provenance.length > 0 && (
            <span className="text-[8px] leading-tight text-muted-foreground/50">
              {provenance.join(" · ")}
            </span>
          )}
          {score.adjustment_reason && score.panic_score_raw != null && (
            <span className="text-[8px] leading-tight text-amber-300/60">
              calibré {score.panic_score_raw.toFixed(0)} → {s.toFixed(0)} · {score.adjustment_reason}
            </span>
          )}
        </div>
      )}
    </div>
  );
}

/** One qualifier cell. Renders an em dash rather than hiding when the model omitted
 *  the field, so the report's shape does not shift between tickers. */
function ReportStat({
  label,
  value,
  suffix,
}: {
  label: string;
  value: number | null;
  suffix: string;
}) {
  return (
    <span className="flex items-center gap-1">
      <span className="text-[8px] uppercase tracking-wide text-muted-foreground/50">
        {label}
      </span>
      {value != null ? (
        <span className="text-foreground/85">
          {value.toFixed(0)}
          {suffix}
        </span>
      ) : (
        <span className="text-muted-foreground/30">—</span>
      )}
    </span>
  );
}

// ─── Micro Pullback info overlay (rich, top-left of the right/sub-minute pane) ─

const clamp01 = (x: number) => Math.max(0, Math.min(1, x));

/** Real-time liquidity fill from the last-60s share volume: log scale, empty at
 *  ≤1K shares/min, full at ≥1M shares/min. */
function volumeFill(v: number): number {
  const lo = Math.log10(1_000);
  const hi = Math.log10(1_000_000);
  return clamp01((Math.log10(Math.max(v, 1)) - lo) / (hi - lo));
}

type Tone = "blue" | "red" | "muted";

const FILL_CLS: Record<Tone, string> = {
  blue:  "bg-sky-400/85",
  red:   "bg-rose-500/85",
  muted: "bg-zinc-500/40",
};

/** One aligned metric row: fixed label · flexible bar · fixed value. `fill` null
 *  → no data (greyed, faded). `value` null → number hidden (abstract 0..100
 *  scores) while the bar still conveys the magnitude. */
function MetricBar({
  label, fill, value, tone,
}: {
  label: string;
  fill:  number | null;
  value: string | null;
  tone:  Tone;
}) {
  const absent = fill == null;
  return (
    <div
      data-capture-cell
      className={cn(
        "grid grid-cols-[62px_minmax(0,1fr)_40px] items-center gap-2",
        absent && "opacity-40",
      )}
    >
      <span
        data-cap-label
        className="truncate text-[9px] uppercase leading-none tracking-wide text-muted-foreground/60"
      >
        {label}
      </span>
      <span className="h-1.5 w-full overflow-hidden rounded-full bg-white/10">
        {!absent && (
          <span
            className={cn("block h-full rounded-full", FILL_CLS[tone])}
            style={{ width: `${Math.round((fill ?? 0) * 100)}%` }}
          />
        )}
      </span>
      <span
        data-cap-value
        className="text-right text-[13px] font-semibold leading-none tabular-nums text-foreground/90"
      >
        {value ?? ""}
      </span>
    </div>
  );
}

/** News freshness badge from an ISO publish time: "20sec" / "1min" / "2h" / "3j",
 *  to the second under a minute. Green < 20 min, orange < 60 min, red beyond. */
function newsFreshness(createdAt: string, nowMs: number): { text: string; cls: string } {
  const ms = Date.parse(createdAt);
  const sec = Number.isFinite(ms) ? Math.max(0, Math.round((nowMs - ms) / 1000)) : 0;
  const text =
    sec < 60 ? `${sec}sec`
    : sec < 3600 ? `${Math.floor(sec / 60)}min`
    : sec < 86_400 ? `${Math.floor(sec / 3600)}h`
    : `${Math.floor(sec / 86_400)}j`;
  const cls = sec < 20 * 60
    ? "bg-emerald-900/70 text-emerald-300"
    : sec < 60 * 60
    ? "bg-amber-900/70 text-amber-300"
    : "bg-red-900/70 text-red-300";
  return { text, cls };
}

/** A row in the news list: freshness badge + headline (2 lines max). */
function NewsRow({ item, nowMs }: { item: CardNews; nowMs: number }) {
  const { text, cls } = newsFreshness(item.created_at, nowMs);
  return (
    <div data-capture-cell className="flex items-start gap-1.5">
      <span
        data-cap-label
        className={cn("mt-px shrink-0 rounded px-1 py-0.5 text-[8px] font-semibold tabular-nums", cls)}
      >
        {text}
      </span>
      <span
        data-cap-value
        className="line-clamp-2 text-[10px] leading-snug text-foreground/80"
        title={item.headline}
      >
        {item.headline}
      </span>
    </div>
  );
}

/** Re-render every `ms` so the live-derived news ages stay accurate to the second. */
function useNowTick(ms: number): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const id = setInterval(() => setNow(Date.now()), ms);
    return () => clearInterval(id);
  }, [ms]);
  return now;
}

/** The instant news ages are measured against: the SIMULATED clock during a Market
 *  Replay, the wall clock otherwise.
 *
 *  Not a detail. Replaying yesterday's premarket today, a headline that is two
 *  minutes old inside the replayed session is nineteen hours old by the wall clock,
 *  and the freshness badge painted it red — "ancienne" — while the model, which
 *  reads the same headline through the replay clock, correctly called it breaking.
 *  The panel and the score have to measure time the same way or they contradict each
 *  other on the one field the A+ decision turns on.
 *
 *  Shares the `replay_status` query key with the session clock, so the extra
 *  consumer costs no extra poll. */
function useMarketNowMs(): number {
  const wall = useNowTick(1000);
  const { data: replay } = useQuery({
    queryKey: ["replay_status"],
    queryFn:  () => api.getReplayStatus(),
    refetchInterval: 1000,
  });
  if (replay?.active && replay.sim_time) {
    const t = Date.parse(replay.sim_time);
    if (Number.isFinite(t)) return t;
  }
  return wall;
}
/** Parse one of the A+ row's JSON-array columns. They cross the bridge as raw text
 *  exactly as stored, so a malformed blob must degrade to "no chips", never to a
 *  blank pane. */
function jsonList(raw: string | null): string[] {
  if (!raw) return [];
  try {
    const v: unknown = JSON.parse(raw);
    return Array.isArray(v) ? v.filter((x): x is string => typeof x === "string") : [];
  } catch {
    return [];
  }
}

/** What the pipeline is doing, in the trader's words. */
const PHASE_LABEL: Record<string, string> = {
  queued:    "en file",
  triage:    "lecture de la news…",
  deep:      "analyse approfondie…",
  rescoring: "re-scoring sur news…",
};

/** News verdict → colour. `aucune` is deliberately NOT red: it is a fact about the
 *  setup, not a fault. It does cap the Score Breaking at 10, so it will already be
 *  visible in the number. */
const NEWS_VERDICT_CLS: Record<string, string> = {
  solide: "text-emerald-300",
  faible: "text-amber-300",
  bluff:  "text-rose-300",
  aucune: "text-muted-foreground/70",
};

/** Dilution risk → colour. `faible` never reaches the screen (see the render). */
const DILUTION_CLS: Record<string, string> = {
  moyen:    "text-amber-300",
  eleve:    "text-rose-300",
  critique: "text-rose-200",
};

/** Verdict colour, keyed on the distance to the trader's own threshold rather than
 *  on absolute bands: the bar is a Settings value, so "good" is relative to it. */
function microTone(score: number, threshold: number): { ring: string; text: string } {
  if (score >= threshold) return { ring: "ring-emerald-500/60", text: "text-emerald-200" };
  if (score >= threshold - 10) return { ring: "ring-amber-500/50", text: "text-amber-200" };
  return { ring: "ring-slate-500/40", text: "text-slate-300" };
}

/** The Micro Pullback A+ report, drawn top-left of the right (sub-minute) pane.
 *
 *  Same information hierarchy as `PanicReportOverlay`, deliberately — the two
 *  strategies put a model verdict on a chart pane and the trader reads both the same
 *  way, in the seconds before deciding:
 *    1. the final score, biggest thing on the pane, against the trader's own bar;
 *    2. the one-sentence explanation — the only prose worth reading in ten seconds;
 *    3. the news state, which for THIS strategy is the subject of the score;
 *    4. the qualifiers — Score Breaking, setup points, confidence;
 *    5. everything else behind a "plus" toggle, including the audit line.
 *
 *  WHAT THE NUMBERS MEAN. The model grades the CATALYST alone and returns the Score
 *  Breaking (0–100). The deterministic setup points — hot industry, risk domicile,
 *  exceptional RVOL, detached attention, tempo depth — are added outside it. Showing
 *  both, and their difference, is the only way a trader can tell "the news is strong"
 *  from "the news is thin but the setup carried it".
 *
 *  NEWS IS NEVER LEFT AMBIGUOUS, because "no headline" and "a headline we have not
 *  read yet" call for opposite decisions and used to look identical (an empty list):
 *    • no catalyst        → said in words, not by an absence;
 *    • scored WITH news   → the model's verdict on it, plus the headline itself;
 *    • headline in flight → the RAW headline and a spinner. That case is common on a
 *      runner — the wire lands after the move starts — and the honest display is the
 *      text the trader can read himself, not a summary computed before it existed.
 *      A score already on screen at that moment is stale by construction, which is
 *      why the spinner also sits on the score.
 *
 *  The five abstract 0..100 risk bars this panel used to draw are gone. Three were the
 *  same subject (dilution capacity / need / history) rendered as unlabelled fills, the
 *  "News score" bar was never wired to anything, and none survived the ten-second
 *  test. With the deep pass on, its `dilution_risk` says the same thing in one word.
 *
 *  With Qualité A+ off, no judgement exists and none is invented: the pane falls back
 *  to the structural facts plus the headlines. */
export function MicroInfoOverlay({
  cardInfo,
  enrichment,
  news,
  quality,
  live,
}: {
  cardInfo: CardInfo | null;
  enrichment: AlertEnrichment | null;
  news: CardNews[];
  quality: MicroQualityRow | null;
  live: MicroQualityLive | null;
}) {
  const [expanded, setExpanded] = useState(false);
  const nowMs = useMarketNowMs();

  const country  = enrichment?.country  ?? cardInfo?.country  ?? null;
  const industry = enrichment?.industry ?? cardInfo?.industry ?? null;
  const float    = enrichment?.float_shares ?? cardInfo?.float_shares ?? null;
  const liveVol  = cardInfo?.live_volume ?? null;

  const phase  = live?.phase ?? null;
  const busy   = phase != null;
  const newest = news.length > 0 ? news[0] : null;

  // A headline the displayed score has not read. Two independent sources, because
  // each catches a case the other misses: the pipeline announces the headline it is
  // re-scoring on (fastest, and the only one that knows a REST-only story), while
  // comparing publication time with `scored_at` catches a headline that landed after
  // the last verdict and has not triggered a re-score yet (the watch is capped at
  // three per session — beyond that the pane must still say the score is behind).
  const unreadFromPipeline = live?.pending_headline ?? null;
  const unreadFromClock =
    newest && quality?.scored_at && Date.parse(newest.created_at) > Date.parse(quality.scored_at)
      ? newest.headline
      : null;
  const unread = unreadFromPipeline ?? unreadFromClock;

  const score     = quality?.final_score ?? null;
  const threshold = quality?.threshold ?? null;
  const passed    = quality?.passed ?? false;
  const flags     = jsonList(quality?.red_flags ?? null).slice(0, 4);
  const drivers   = jsonList(quality?.key_drivers ?? null);

  // The model's own number, and what the deterministic terms did to it. Derived
  // rather than stored: the difference IS the adjustment total, and `adjustments`
  // already carries the term-by-term audit line for the expanded view.
  const breaking =
    quality == null ? null : quality.model === "deep" ? quality.pass2_score : quality.pass1_score;
  const setupDelta =
    breaking != null && score != null ? Math.round(score - breaking) : null;

  const hasAplus = quality != null || busy;
  if (!hasAplus && float == null && liveVol == null && news.length === 0 && !country && !industry) {
    return null;
  }

  const tone = score != null && threshold != null ? microTone(score, threshold) : null;
  const hasComplementary =
    flags.length > 0
    || drivers.length > 0
    || quality?.catalyst_label != null
    || quality?.dilution_risk != null
    || quality?.dilution_read != null
    || quality?.entry_note != null
    || quality?.adjustments != null
    || float != null
    || liveVol != null
    || country != null
    || industry != null;

  return (
    <div
      data-capture-overlay
      className="pointer-events-none absolute left-1.5 top-1.5 z-10 flex w-[300px] max-w-[68%] flex-col gap-1 rounded bg-black/55 px-2 py-1.5 backdrop-blur-[2px]"
    >
      {/* ── 1. The verdict ─────────────────────────────────────────────────── */}
      {hasAplus && (
        <div className="flex items-baseline gap-2">
          {score != null ? (
            <span
              className={cn(
                "rounded px-2 py-0.5 text-[26px] font-bold leading-none tabular-nums ring-1",
                tone?.ring ?? "ring-slate-500/40",
                tone?.text ?? "text-slate-300",
              )}
            >
              {score.toFixed(0)}
            </span>
          ) : (
            <span className="flex items-center gap-1.5 rounded px-2 py-1 text-slate-300 ring-1 ring-slate-500/40">
              <LoaderCircle className="h-4 w-4 animate-spin" />
              <span className="text-[11px] font-semibold">A+</span>
            </span>
          )}
          <span className="flex min-w-0 flex-col gap-0.5">
            <span
              className={cn(
                "text-[11px] font-medium leading-tight",
                score == null ? "text-sky-300" : (tone?.text ?? "text-slate-300"),
              )}
            >
              {score == null
                ? (PHASE_LABEL[phase ?? ""] ?? "analyse en cours…")
                : passed
                ? `retenu · seuil ${threshold != null ? threshold.toFixed(0) : "—"}`
                : (quality?.rejection ?? "sous le seuil")}
            </span>
            {score != null && busy && (
              <span className="flex items-center gap-1 text-[9px] leading-none text-sky-400">
                <LoaderCircle className="h-2.5 w-2.5 animate-spin" />
                {PHASE_LABEL[phase ?? ""] ?? "re-scoring…"}
              </span>
            )}
          </span>
        </div>
      )}

      {/* ── 2. Why ─────────────────────────────────────────────────────────── */}
      {quality?.one_liner && (
        <p className="text-[11px] leading-snug text-foreground/90">{quality.one_liner}</p>
      )}

      {/* ── 3. News: unread / judged / absent — never ambiguous ────────────── */}
      {unread ? (
        <div className="flex items-start gap-1.5 rounded bg-amber-500/10 px-1.5 py-1 ring-1 ring-amber-500/30">
          <LoaderCircle className="mt-px h-3 w-3 shrink-0 animate-spin text-amber-300" />
          <span className="flex min-w-0 flex-col gap-0.5">
            <span className="text-[9px] font-semibold uppercase leading-none tracking-wide text-amber-300">
              {busy ? "news en cours d'analyse" : "news postérieure au score"}
            </span>
            <span className="text-[11px] leading-snug text-amber-100/90">{unread}</span>
          </span>
        </div>
      ) : quality?.news_verdict ? (
        <div className="flex flex-col gap-0.5">
          <div className="flex items-baseline gap-1.5">
            <span
              className={cn(
                "text-[11px] font-semibold leading-tight",
                NEWS_VERDICT_CLS[quality.news_verdict] ?? "text-foreground/80",
              )}
            >
              {quality.news_verdict === "aucune"
                ? "Aucun catalyseur"
                : `News ${quality.news_verdict}`}
            </span>
            {quality.news_freshness && quality.news_freshness !== "aucune" && (
              <span className="text-[9px] uppercase tracking-wide text-muted-foreground/60">
                {quality.news_freshness}
              </span>
            )}
          </div>
          {/* The headline itself when the model gave no label — never both, they say
              the same thing twice. */}
          {!quality.catalyst_label && newest && <NewsRow item={newest} nowMs={nowMs} />}
        </div>
      ) : newest ? (
        <NewsRow item={newest} nowMs={nowMs} />
      ) : (
        <span className="text-[10px] uppercase tracking-wide text-muted-foreground/50">
          Aucune news
        </span>
      )}

      {/* ── 4. The qualifiers ──────────────────────────────────────────────── */}
      {quality != null && (
        <div className="flex flex-wrap items-center gap-x-2.5 gap-y-0.5 text-[10px] tabular-nums">
          <ReportStat label="Breaking" value={breaking} suffix="/100" />
          {/* Signed on purpose: "+25" and "−25" are opposite readings of the same
              candidate, and an unsigned number hides which one happened. */}
          <span className="flex items-center gap-1">
            <span className="text-[8px] uppercase tracking-wide text-muted-foreground/50">
              Setup
            </span>
            {setupDelta != null ? (
              <span
                className={cn(
                  setupDelta > 0 ? "text-emerald-300"
                  : setupDelta < 0 ? "text-rose-300"
                  : "text-foreground/85",
                )}
              >
                {setupDelta > 0 ? "+" : ""}
                {setupDelta}
              </span>
            ) : (
              <span className="text-muted-foreground/30">—</span>
            )}
          </span>
          <ReportStat label="Confiance" value={quality.confidence} suffix="/100" />
          <ReportStat label="Tenue" value={quality.sustain_prob} suffix="%" />
        </div>
      )}

      {/* ── Toggle → complementary info ────────────────────────────────────── */}
      {hasComplementary && (
        <button
          type="button"
          onClick={() => setExpanded((v) => !v)}
          className="pointer-events-auto flex items-center gap-1 self-start text-[9px] uppercase tracking-wide text-muted-foreground/60 hover:text-foreground/80"
        >
          <ChevronRight className={cn("h-2.5 w-2.5 transition-transform", expanded && "rotate-90")} />
          {expanded ? "moins" : "plus"}
        </button>
      )}

      {expanded && (
        <div className="flex flex-col gap-1 border-t border-white/10 pt-1">
          {quality?.catalyst_label && (
            <span className="text-[10px] text-foreground/70">{quality.catalyst_label}</span>
          )}

          {(flags.length > 0 || (quality?.dilution_risk && quality.dilution_risk !== "faible")) && (
            <div className="flex flex-wrap items-center gap-1">
              {quality?.dilution_risk && quality.dilution_risk !== "faible" && (
                <span
                  className={cn(
                    "rounded bg-white/5 px-1 py-0.5 text-[9px] font-semibold uppercase tracking-wide",
                    DILUTION_CLS[quality.dilution_risk] ?? "text-amber-300",
                  )}
                  title={quality.dilution_read ?? undefined}
                >
                  dilution {quality.dilution_risk}
                </span>
              )}
              {flags.map((f) => (
                <span
                  key={f}
                  className="rounded bg-white/5 px-1 py-0.5 text-[9px] leading-tight text-rose-200/80"
                >
                  {f}
                </span>
              ))}
            </div>
          )}

          {drivers.length > 0 && (
            <div className="flex flex-wrap gap-1">
              {drivers.map((d) => (
                <span
                  key={d}
                  className="rounded bg-white/10 px-1.5 py-0.5 text-[9px] leading-none text-foreground/80"
                >
                  {d}
                </span>
              ))}
            </div>
          )}

          {quality?.entry_note && (
            <span className="text-[10px] leading-snug text-muted-foreground">
              ⚑ {quality.entry_note}
            </span>
          )}

          {(float != null || liveVol != null || country || industry) && (
            <div className="flex flex-wrap items-baseline gap-x-2 gap-y-0.5 text-[10px] text-muted-foreground/70">
              {float != null && (
                <span className="tabular-nums">
                  float <span className="font-semibold text-foreground/80">{fmtCompact(float)}</span>
                </span>
              )}
              {liveVol != null && (
                <span className="tabular-nums">
                  vol/min{" "}
                  <span className="font-semibold text-foreground/80">{fmtCompact(liveVol)}</span>
                </span>
              )}
              {(country || industry) && (
                <span className="truncate" title={[country, industry].filter(Boolean).join(" · ")}>
                  {[country, industry].filter(Boolean).join(" · ")}
                </span>
              )}
            </div>
          )}

          {/* The audit line: every deterministic term that moved the number, plus
              which pass spoke and under which rubric. A score adjusted by +25 must
              never be silently different from what the model said. */}
          {quality && (
            <span className="text-[8px] leading-tight text-muted-foreground/50">
              {[
                quality.model === "deep" ? "double passe" : "triage seul",
                quality.adjustments,
                quality.rubric_version,
                quality.simulated ? "replay" : null,
              ]
                .filter(Boolean)
                .join(" · ")}
            </span>
          )}
        </div>
      )}
    </div>
  );
}

// ─── HOD Drive info overlay (5 KPIs, top-left of the right/timeframe pane) ────

/** The HOD Drive overlay: the five key numbers of a clean post-open drive + its
 *  pullback toward the HOD, drawn as aligned bars (same look as MicroInfoOverlay).
 *  All from the live `get_hod_drive_overlay` recompute; greys a row when its value
 *  isn't available yet. The HOD/LOD points + green-series crosses are drawn on the
 *  candles themselves (chart markers), not here. */
export function HodDriveInfoOverlay({ overlay, draftRr, onConfirm, invertRr, onInvert }: {
  overlay: HodDriveOverlay | null;
  draftRr?: number | null;
  onConfirm?: () => void;
  /** R:R of the mirrored (short) trade — the reciprocal of `draftRr`. */
  invertRr?: number | null;
  onInvert?: () => void;
}) {
  const share  = overlay?.series_share ?? null;          // 0..1
  const pbVol  = overlay?.pullback_volume ?? null;       // shares
  const ratio  = overlay?.pullback_vol_ratio ?? null;    // 1.0 = equal
  const power  = overlay?.power_score ?? null;            // 0..1
  const eff    = overlay?.directional_efficiency ?? null; // 0..1
  const rgAtr  = overlay?.range_vs_green_atr ?? null;    // 1.0 = 100%
  const mOpen  = overlay?.macd_open ?? null;              // true/false/null
  const mStr   = overlay?.macd_strength ?? null;          // 0..1
  const pct = (x: number) => `${Math.round(x * 100)}%`;

  return (
    <div
      data-capture-overlay
      className="pointer-events-none absolute left-1.5 top-1.5 z-10 flex w-[228px] max-w-[78%] flex-col gap-1.5 rounded bg-black/40 px-2 py-1.5 backdrop-blur-[1px]"
    >
      <div data-capture-cell className="flex items-center justify-between leading-none">
        <span data-cap-label className="text-[9px] uppercase tracking-wide text-muted-foreground/60">
          HOD Drive {overlay?.timeframe ?? ""}
        </span>
        {overlay?.gates_pass && (
          <span data-cap-value className="rounded bg-emerald-900/70 px-1 py-0.5 text-[8px] font-semibold text-emerald-300">
            OK
          </span>
        )}
      </div>
      <div className="h-px w-full bg-white/10" />

      {/* Green series range as a share of the open range (HOD−LOD). */}
      <MetricBar
        label="Série/range"
        fill={share != null ? clamp01(share) : null}
        value={share != null ? pct(share) : null}
        tone="blue"
      />
      {/* Pullback bars' volume (absolute) + its size vs the green series' volume. */}
      <MetricBar
        label="Vol pullbk"
        fill={pbVol != null ? volumeFill(pbVol) : null}
        value={pbVol != null ? fmtCompact(pbVol) : null}
        tone="blue"
      />
      <MetricBar
        label="Vol PB/sér"
        fill={ratio != null ? clamp01(ratio / 2) : null}
        value={ratio != null ? pct(ratio) : null}
        tone={ratio != null && ratio > 1 ? "red" : "blue"}
      />
      {/* Body-dominance of the series (quality of the move). */}
      <MetricBar
        label="Power"
        fill={power != null ? clamp01(power) : null}
        value={power != null ? pct(power) : null}
        tone="blue"
      />
      {/* Directional efficiency: ~1 clean, ~0 choppy. */}
      <MetricBar
        label="Efficience"
        fill={eff != null ? clamp01(eff) : null}
        value={eff != null ? pct(eff) : null}
        tone="blue"
      />
      {/* Today's HOD/LOD range vs average range of green daily candles. */}
      <MetricBar
        label="Rg/ATR vert"
        fill={rgAtr != null ? clamp01(rgAtr / 2) : null}
        value={rgAtr != null ? pct(rgAtr) : null}
        tone={rgAtr != null && rgAtr >= 1 ? "red" : "blue"}
      />
      {/* MACD trend status: open (blue) = momentum healthy, closed (red) = exhausted. */}
      <MetricBar
        label="MACD"
        fill={mStr != null ? clamp01(mStr) : null}
        value={mOpen != null ? (mOpen ? "Ouvert" : "Fermé") : null}
        tone={mOpen === true ? "blue" : "red"}
      />
      {((draftRr != null && onConfirm) || (invertRr != null && onInvert)) && (
        <>
          <div className="h-px w-full bg-white/10" />
          {draftRr != null && onConfirm && (
            <button
              onClick={onConfirm}
              className={cn(
                "pointer-events-auto w-full rounded bg-amber-600/70 px-2 py-1 text-[10px] font-semibold",
                "text-white transition-colors hover:bg-amber-500/80 active:bg-amber-400/80",
              )}
            >
              Confirmer — {draftRr.toFixed(1)}R
            </button>
          )}
          {/* Mirror trade: same suggested prices, roles swapped (TP→SL, SL→TP),
              taken short. Rose = the overlay's "red" tone (see FILL_CLS). */}
          {invertRr != null && onInvert && (
            <button
              onClick={onInvert}
              className={cn(
                "pointer-events-auto w-full rounded bg-rose-600/70 px-2 py-1 text-[10px] font-semibold",
                "text-white transition-colors hover:bg-rose-500/80 active:bg-rose-400/80",
              )}
            >
              Inverser — {invertRr.toFixed(1)}R
            </button>
          )}
        </>
      )}
    </div>
  );
}

// ─── Small toolbar button ─────────────────────────────────────────────────────

export function TBtn({
  children,
  onClick,
  disabled = false,
  active = false,
  title,
  className,
}: {
  children: React.ReactNode;
  onClick?: () => void;
  disabled?: boolean;
  active?: boolean;
  title?: string;
  className?: string;
}) {
  return (
    <button
      title={title}
      disabled={disabled}
      onClick={onClick}
      className={cn(
        "flex h-5 shrink-0 items-center gap-0.5 rounded px-1.5 text-[10px] font-medium transition-colors",
        disabled
          ? "cursor-not-allowed text-muted-foreground/20"
          : active
          ? "bg-accent text-foreground"
          : "text-muted-foreground hover:bg-accent hover:text-foreground",
        className
      )}
    >
      {children}
    </button>
  );
}

export function Sep() {
  return <div className="mx-0.5 h-3 w-px shrink-0 bg-border/60" />;
}

// ─── Pending text annotation input ────────────────────────────────────────────

export function TextInput({
  onConfirm,
  onCancel,
}: {
  onConfirm: (text: string) => void;
  onCancel:  () => void;
}) {
  const [val, setVal] = useState("");
  return (
    <div className="flex items-center gap-1 rounded border border-amber-700/60 bg-zinc-900 px-1.5 py-1 shadow-lg">
      <input
        autoFocus
        value={val}
        onChange={(e) => setVal(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" && val.trim()) onConfirm(val.trim());
          if (e.key === "Escape") onCancel();
        }}
        placeholder="Annotation…"
        className="w-28 bg-transparent text-[10px] text-amber-300 placeholder-muted-foreground/40 outline-none"
      />
      <button
        onClick={() => val.trim() && onConfirm(val.trim())}
        className="text-[10px] text-amber-400 hover:text-amber-200"
      >
        ✓
      </button>
    </div>
  );
}

// ─── Empty zone ───────────────────────────────────────────────────────────────

export function EmptyZone({ zone }: { zone: ZoneAssignment }) {
  return (
    <div
      data-zone-id={zone.zone_id}
      className="flex h-full w-full flex-col rounded-md border border-dashed border-border/50 bg-card/10"
    >
      <div className="flex flex-1 flex-col items-center justify-center gap-1.5 select-none">
        <span className="text-xs text-muted-foreground/40">Zone vide</span>
        <span className="text-[10px] text-muted-foreground/25">
          Sélectionne un ticker dans les alertes
        </span>
      </div>
    </div>
  );
}
