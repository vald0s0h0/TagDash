// Presentational helpers extracted from ChartZone: priority styling, info-band
// field formatting/rendering, and the small toolbar widgets. All are pure /
// self-contained (no shared refs, no ChartZone state), so they live here to keep
// ChartZone focused on orchestration.

import { useEffect, useState } from "react";
import { LoaderCircle, Sparkles } from "lucide-react";
import type {
  AlertEnrichment, CardInfo, CardNews, HodDriveOverlay, InfoField, StrategyCard,
  TickerLiveState, Timeframe, ZoneAssignment,
} from "@/types";
import { cn } from "@/lib/utils";

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
function BarChip({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <span className="flex shrink-0 items-center gap-1">
      <span className="text-[8px] uppercase tracking-wide text-muted-foreground/45">{label}</span>
      <span className="flex items-center gap-0.5 text-[11px] tabular-nums text-foreground/85">
        {children}
      </span>
    </span>
  );
}

/** The shared info bar shown above every chart, identical for all strategies:
 *  strategy badge · Bollinger Z · premarket volume · current-bar volume · news
 *  presence · IA news-analysis button · context/verdict. Strategy-specific fields
 *  live in the on-chart overlay (StrategyInfoOverlay), not here. */
export function ChartInfoBar({
  zone,
  card,
  cardInfo,
  enrichment,
  dayVolume,
  currentBarVolume,
  onRunLlm,
}: {
  zone: ZoneAssignment;
  card: StrategyCard | null;
  cardInfo: CardInfo | null;
  enrichment: AlertEnrichment | null;
  dayVolume: number | null;
  currentBarVolume: number | null;
  onRunLlm: () => void;
}) {
  const bbz = cardInfo?.bbz ?? null;
  // BBZ colours by stretch: amber past ±2σ, red past ±3σ.
  const bbzCls = bbz == null ? "" : Math.abs(bbz) >= 3
    ? "text-red-400" : Math.abs(bbz) >= 2 ? "text-amber-400" : "text-foreground/85";

  const newsTitle = cardInfo?.news_title ?? enrichment?.news_title ?? null;
  const newsChecked = enrichment?.news_checked ?? false;

  // LLM context/verdict, sourced uniformly across strategies (panic: context /
  // reversion; micro: dilution / news bluff). Button shown when the strategy
  // declares an LLM call.
  const hasLlm  = !!card?.llm;
  const pending = enrichment?.llm_pending ?? false;
  const context = enrichment ? (enrichment.llm_context  ?? enrichment.llm_dilution) : null;
  const verdict = enrichment ? (enrichment.llm_reversion ?? enrichment.llm_news)    : null;
  const hasResult = context != null || verdict != null;

  return (
    <div className="flex flex-wrap items-center gap-x-3 gap-y-0.5 px-2 pt-1">
      <StrategyBadge strategyId={zone.strategy_id} name={zone.strategy_name} />

      {/* Bollinger Z of the live price (daily basis) */}
      <BarChip label="BBZ">
        {bbz != null
          ? <span className={bbzCls}>{bbz >= 0 ? "+" : ""}{bbz.toFixed(2)}σ</span>
          : <span className="text-muted-foreground/30">—</span>}
      </BarChip>

      {/* Premarket cumulative volume */}
      <BarChip label="Vol PM">
        {cardInfo?.premarket_volume != null
          ? fmtCompact(cardInfo.premarket_volume)
          : <span className="text-muted-foreground/30">—</span>}
      </BarChip>

      {/* Cumulative volume traded so far today — live */}
      <BarChip label="Vol Day">
        {dayVolume != null
          ? fmtCompact(dayVolume)
          : <span className="text-muted-foreground/30">—</span>}
      </BarChip>

      {/* Current (forming) bar volume — live */}
      <BarChip label="Vol barre">
        {currentBarVolume != null
          ? fmtCompact(currentBarVolume)
          : <span className="text-muted-foreground/30">—</span>}
      </BarChip>

      {/* News presence */}
      <BarChip label="News">
        {newsTitle ? (
          <span className="max-w-[220px] truncate" title={newsTitle}>{newsTitle}</span>
        ) : newsChecked ? (
          <span className="rounded bg-red-900/60 px-1 py-0.5 text-[9px] font-semibold text-red-300">
            aucune
          </span>
        ) : cardInfo?.has_news === false ? (
          <span className="text-muted-foreground/40">aucune</span>
        ) : (
          <span className="text-muted-foreground/30">—</span>
        )}
      </BarChip>

      {/* IA news-analysis button + context/verdict (strategies with an LLM) */}
      {hasLlm && (
        <>
          <button
            type="button"
            onClick={onRunLlm}
            disabled={pending}
            title="Analyser les news et le contexte (IA)"
            className={cn(
              "flex items-center gap-1 rounded px-1.5 py-0.5 text-[9px] font-semibold",
              "bg-blue-900/50 text-blue-200 hover:bg-blue-800/60 disabled:opacity-50",
            )}
          >
            {pending ? (
              <LoaderCircle className="h-2.5 w-2.5 animate-spin" />
            ) : (
              <Sparkles className="h-2.5 w-2.5" />
            )}
            {hasResult ? "Réanalyser" : "Analyse IA"}
          </button>
          {(context || pending) && (
            <BarChip label="Contexte">
              <span className="max-w-[260px] truncate" title={context ?? undefined}>
                {context ?? <LoaderCircle className="h-2.5 w-2.5 animate-spin text-blue-400/70" />}
              </span>
            </BarChip>
          )}
          {(verdict || pending) && (
            <BarChip label="Verdict">
              <span className="max-w-[260px] truncate" title={verdict ?? undefined}>
                {verdict ?? <LoaderCircle className="h-2.5 w-2.5 animate-spin text-blue-400/70" />}
              </span>
            </BarChip>
          )}
        </>
      )}

      {zone.price != null && (
        <span className="ml-auto shrink-0 text-xs font-medium tabular-nums">
          ${zone.price.toFixed(2)}
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

// ─── Micro Pullback info overlay (rich, top-left of the right/sub-minute pane) ─

const clamp01 = (x: number) => Math.max(0, Math.min(1, x));

/** Float "tightness" fill: a LOWER float reads as a fuller bar (more explosive —
 *  the whole point of a low-float scanner). Plain linear scale, full at ≤500K,
 *  empty at ≥30M (the low-float ceiling). The exact share count is shown
 *  alongside, so the bar is the at-a-glance. */
function floatFill(f: number): number {
  const lo = 500_000;
  const hi = 30_000_000;
  return clamp01(1 - (f - lo) / (hi - lo));
}

/** Real-time liquidity fill from the last-60s share volume: log scale, empty at
 *  ≤1K shares/min, full at ≥1M shares/min. */
function volumeFill(v: number): number {
  const lo = Math.log10(1_000);
  const hi = Math.log10(1_000_000);
  return clamp01((Math.log10(Math.max(v, 1)) - lo) / (hi - lo));
}

/** A 0..100 score as a 0..1 bar fill (clamped). */
const scoreFill = (s: number) => clamp01(s / 100);

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

/** The Micro Pullback overlay: a compact, at-a-glance risk panel drawn top-left
 *  of the right (sub-minute) pane. Keeps the generic overlay's look (transparent
 *  dark backdrop, muted uppercase labels, semibold tabular values) but lays the
 *  values out as aligned horizontal bars: blue = liquidity / interest (float,
 *  premarket volume, short interest), red = dilution / manipulation risk, plus a
 *  (not-yet-wired) news-score placeholder and the most recent headlines with a
 *  freshness badge. Missing data greys its row out. */
export function MicroInfoOverlay({
  cardInfo,
  enrichment,
  news,
}: {
  cardInfo: CardInfo | null;
  enrichment: AlertEnrichment | null;
  news: CardNews[];
}) {
  const nowMs = useNowTick(1000);

  const country  = enrichment?.country  ?? cardInfo?.country  ?? null;
  const industry = enrichment?.industry ?? cardInfo?.industry ?? null;
  const float    = enrichment?.float_shares ?? cardInfo?.float_shares ?? null;
  const liveVol  = cardInfo?.live_volume ?? null;

  return (
    <div
      data-capture-overlay
      className="pointer-events-none absolute left-1.5 top-1.5 z-10 flex w-[246px] max-w-[78%] flex-col gap-1.5 rounded bg-black/40 px-2 py-1.5 backdrop-blur-[1px]"
    >
      {/* Pays · Industrie */}
      <div data-capture-cell className="flex items-baseline gap-1.5 leading-none">
        <span data-cap-label className="hidden" />
        <span
          data-cap-value
          className="truncate text-[11px] font-semibold text-foreground/90"
          title={[country, industry].filter(Boolean).join(" · ")}
        >
          {country || industry ? (
            <>
              {country ?? "—"}
              {industry && (
                <span className="font-normal text-muted-foreground/70"> · {industry}</span>
              )}
            </>
          ) : (
            <span className="text-muted-foreground/40">Pays · Industrie</span>
          )}
        </span>
      </div>

      <div className="h-px w-full bg-white/10" />

      {/* Liquidity / interest — blue (numbers shown for float & volume) */}
      <MetricBar
        label="Float"
        fill={float != null ? floatFill(float) : null}
        value={float != null ? fmtCompact(float) : null}
        tone="blue"
      />
      <MetricBar
        label="Vol/min"
        fill={liveVol != null ? volumeFill(liveVol) : null}
        value={liveVol != null ? fmtCompact(liveVol) : null}
        tone="blue"
      />
      <MetricBar
        label="Short int"
        fill={cardInfo?.short_interest_score != null ? scoreFill(cardInfo.short_interest_score) : null}
        value={null}
        tone="blue"
      />

      {/* Dilution / manipulation risk — red (abstract scores, bar only) */}
      <MetricBar
        label="Capa. dil."
        fill={cardInfo?.dilution_capacity_score != null ? scoreFill(cardInfo.dilution_capacity_score) : null}
        value={null}
        tone="red"
      />
      <MetricBar
        label="Besoin dil."
        fill={cardInfo?.dilution_need_score != null ? scoreFill(cardInfo.dilution_need_score) : null}
        value={null}
        tone="red"
      />
      <MetricBar
        label="Dil. hist."
        fill={cardInfo?.dilution_score != null ? scoreFill(cardInfo.dilution_score) : null}
        value={null}
        tone="red"
      />
      <MetricBar
        label="Pump&Dump"
        fill={cardInfo?.pump_dump_score != null ? scoreFill(cardInfo.pump_dump_score) : null}
        value={null}
        tone="red"
      />

      {/* News score — placeholder bar, wired in a later iteration. */}
      <MetricBar label="News score" fill={null} value={null} tone="muted" />

      {/* Recent headlines with a freshness badge (up to 4, newest first). */}
      <div className="h-px w-full bg-white/10" />
      <div className="flex flex-col gap-1">
        {news.length > 0 ? (
          news.slice(0, 4).map((n, i) => <NewsRow key={`${n.created_at}-${i}`} item={n} nowMs={nowMs} />)
        ) : (
          <span className="text-[9px] uppercase tracking-wide text-muted-foreground/40">
            Aucune news
          </span>
        )}
      </div>
    </div>
  );
}

// ─── HOD Drive info overlay (5 KPIs, top-left of the right/timeframe pane) ────

/** The HOD Drive overlay: the five key numbers of a clean post-open drive + its
 *  pullback toward the HOD, drawn as aligned bars (same look as MicroInfoOverlay).
 *  All from the live `get_hod_drive_overlay` recompute; greys a row when its value
 *  isn't available yet. The HOD/LOD points + green-series crosses are drawn on the
 *  candles themselves (chart markers), not here. */
export function HodDriveInfoOverlay({ overlay, draftRr, onConfirm }: {
  overlay: HodDriveOverlay | null;
  draftRr?: number | null;
  onConfirm?: () => void;
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
      {draftRr != null && onConfirm && (
        <>
          <div className="h-px w-full bg-white/10" />
          <button
            onClick={onConfirm}
            className={cn(
              "pointer-events-auto w-full rounded bg-amber-600/70 px-2 py-1 text-[10px] font-semibold",
              "text-white transition-colors hover:bg-amber-500/80 active:bg-amber-400/80",
            )}
          >
            Confirmer — {draftRr.toFixed(1)}R
          </button>
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
