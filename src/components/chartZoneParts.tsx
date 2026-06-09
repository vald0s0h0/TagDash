// Presentational helpers extracted from ChartZone: priority styling, info-band
// field formatting/rendering, and the small toolbar widgets. All are pure /
// self-contained (no shared refs, no ChartZone state), so they live here to keep
// ChartZone focused on orchestration.

import { useState } from "react";
import { LoaderCircle } from "lucide-react";
import type { CardInfo, InfoField, TickerLiveState, Timeframe, ZoneAssignment } from "@/types";
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

/** Resolve a strategy-card info-field key to a formatted value, or null when not
 *  (yet) available. `extras` carries per-symbol data not in the live snapshot
 *  (score / market cap / float, from get_card_info); everything else comes from
 *  the live snapshot (TickerLiveState). Keys that resolve to null show "—" (alert
 *  source) or a spinner (llm/enrichment source). */
export function resolveFieldValue(
  key: string,
  live: TickerLiveState | null,
  extras: CardInfo | null,
): string | null {
  // Extras-sourced fields (mean-reversion score, market cap, float).
  switch (key) {
    case "mr_score":
      if (extras?.mr_score == null) return null;
      return [
        extras.mr_score_kind ?? "",
        String(Math.round(extras.mr_score)),
        extras.mr_best_days != null ? `· ${extras.mr_best_days}j` : "",
      ].filter(Boolean).join(" ");
    case "market_cap":
      return extras?.market_cap != null ? fmtMoney(extras.market_cap) : null;
    case "float_shares":
      return extras?.float_shares != null ? fmtCompact(extras.float_shares) : null;
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

export function EmptyZone({
  zone,
  onDrop,
}: {
  zone: ZoneAssignment;
  onDrop: (e: React.DragEvent) => void;
}) {
  const [over, setOver] = useState(false);
  return (
    <div
      className={cn(
        "flex h-full w-full flex-col rounded-md border border-dashed border-border/50 bg-card/10 transition-colors",
        over && "border-blue-500/70 bg-blue-900/10"
      )}
      onDragOver={(e) => { e.preventDefault(); setOver(true); }}
      onDragLeave={(e) => { if (!e.currentTarget.contains(e.relatedTarget as Node)) setOver(false); }}
      onDrop={(e) => { setOver(false); onDrop(e); }}
    >
      <div className="flex flex-1 flex-col items-center justify-center gap-1.5 select-none">
        <span className="text-xs text-muted-foreground/40">Zone vide</span>
        <span className="text-[10px] text-muted-foreground/25">Glisser une alerte ici</span>
      </div>
    </div>
  );
}
