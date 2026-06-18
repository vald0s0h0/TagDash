import { X } from "lucide-react";
import type { AlertSignal, ScreenerMatch } from "@/types";
import { useUiStore } from "@/stores/uiStore";
import { useLayoutStore } from "@/stores/layoutStore";
import { cn } from "@/lib/utils";

// ─── Helpers ──────────────────────────────────────────────────────────────────

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
 *  (panes / indicators / info band) exactly like a real alert placement. */
function matchToAlert(m: ScreenerMatch): AlertSignal {
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

// ─── Single screener card ───────────────────────────────────────────────────────

interface CardProps {
  match: ScreenerMatch;
  selected: boolean;
  onOpen: (m: ScreenerMatch) => void;
  onDismiss: (symbol: string) => void;
}

function ScreenerCard({ match: m, selected, onOpen, onDismiss }: CardProps) {
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
      {/* Dismiss button */}
      <button
        title="Retirer de la liste"
        onClick={(e) => { e.stopPropagation(); onDismiss(m.symbol); }}
        className="absolute right-1.5 top-1.5 rounded p-0.5 text-muted-foreground/40 opacity-0 transition-opacity hover:bg-accent hover:text-red-400 group-hover:opacity-100"
      >
        <X className="h-3.5 w-3.5" />
      </button>

      {/* Symbol + price + score badge */}
      <div className="flex items-baseline gap-2">
        <span className="text-sm font-semibold tabular-nums">{m.symbol}</span>
        {m.price != null && (
          <span className="text-xs tabular-nums text-foreground/80">${m.price.toFixed(2)}</span>
        )}
        {m.score != null && (
          <span
            title="Score mean-reversion (PR = percent rank momentum · BB = anomalie Bollinger)"
            className="ml-auto mr-4 rounded bg-violet-900/50 px-1.5 py-0.5 text-[10px] font-bold tabular-nums text-violet-200"
          >
            {m.score_label ?? m.score.toFixed(0)}
          </span>
        )}
      </div>

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

      {/* Strategy chip */}
      <div className="mt-1.5">
        <span className="rounded bg-blue-900/40 px-1.5 py-0.5 text-[9px] font-medium text-blue-300">
          {m.strategy_name}
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

  const visible = matches.filter((m) => !dismissed.includes(m.symbol));

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
      {visible.map((m) => (
        <ScreenerCard
          key={`${m.symbol}-${m.strategy_id}`}
          match={m}
          selected={selectedTicker === m.symbol}
          onOpen={open}
          onDismiss={dismissScreener}
        />
      ))}
    </div>
  );
}
