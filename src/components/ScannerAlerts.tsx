import { GripVertical } from "lucide-react";
import type { AlertSignal } from "@/types";
import { useUiStore } from "@/stores/uiStore";
import { useLayoutStore } from "@/stores/layoutStore";
import { cn } from "@/lib/utils";

// ─── Priority colours ─────────────────────────────────────────────────────────

const PRIORITY_STYLES: Record<number, { badge: string; row: string }> = {
  1: {
    badge: "bg-zinc-700 text-zinc-300",
    row:   "border-l-2 border-zinc-700",
  },
  2: {
    badge: "bg-blue-900/60 text-blue-300",
    row:   "border-l-2 border-blue-700",
  },
  3: {
    badge: "bg-amber-900/60 text-amber-300",
    row:   "border-l-2 border-amber-500",
  },
  4: {
    badge: "bg-orange-900/60 text-orange-300",
    row:   "border-l-2 border-orange-500",
  },
  5: {
    badge: "bg-red-900/70 text-red-300 animate-pulse",
    row:   "border-l-2 border-red-500",
  },
};

const PRIORITY_LABELS: Record<number, string> = {
  1: "P1",
  2: "P2",
  3: "P3",
  4: "P4",
  5: "P5 !!",
};

// ─── Helpers ──────────────────────────────────────────────────────────────────

function fmt(v: number | null, decimals = 2): string {
  return v != null ? v.toFixed(decimals) : "—";
}

function fmtVol(v: number | null): string {
  if (v == null) return "—";
  if (v >= 1_000_000) return `${(v / 1_000_000).toFixed(1)}M`;
  if (v >= 1_000)     return `${(v / 1_000).toFixed(0)}K`;
  return String(v);
}

function fmtFloat(v: number | null): string {
  if (v == null) return "";
  if (v >= 1_000_000) return `${(v / 1_000_000).toFixed(1)}M fl`;
  return `${(v / 1_000).toFixed(0)}K fl`;
}

function relTime(iso: string): string {
  try {
    const diff = Date.now() - new Date(iso).getTime();
    if (diff < 60_000) return `${Math.round(diff / 1000)}s ago`;
    if (diff < 3_600_000) return `${Math.round(diff / 60_000)}m ago`;
    return `${Math.round(diff / 3_600_000)}h ago`;
  } catch {
    return "";
  }
}

// ─── Single alert row ─────────────────────────────────────────────────────────

interface AlertRowProps {
  alert: AlertSignal;
  selected: boolean;
  onOpen: (alert: AlertSignal) => void;
}

function AlertRow({ alert: a, selected, onOpen }: AlertRowProps) {
  const styles = PRIORITY_STYLES[a.priority] ?? PRIORITY_STYLES[1];

  return (
    // draggable — drag-and-drop to chart zone (data carried in dataTransfer)
    <li
      className={cn(
        "group relative cursor-pointer px-3 py-2 hover:bg-accent/40 transition-colors",
        styles.row,
        selected && "bg-accent/60",
      )}
      draggable
      onClick={() => onOpen(a)}
      onDragStart={(e) => {
        // Send full AlertSignal so drop targets can call placeAlertInZone directly
        e.dataTransfer.setData("application/tagdash-alert", JSON.stringify(a));
        e.dataTransfer.effectAllowed = "copy";
      }}
    >
      {/* Drag handle */}
      <span className="absolute left-0 top-1/2 -translate-y-1/2 opacity-0 group-hover:opacity-40">
        <GripVertical className="h-3.5 w-3.5" />
      </span>

      {/* Row header */}
      <div className="flex items-center justify-between gap-1">
        <span className="text-sm font-semibold tabular-nums">{a.symbol}</span>
        <div className="flex items-center gap-1.5">
          {a.display_timeframe && (
            <span className="rounded bg-violet-900/50 px-1 py-0.5 text-[9px] font-bold uppercase text-violet-300">
              {a.display_timeframe}
            </span>
          )}
          {a.side && (
            <span className={cn(
              "rounded px-1 py-0.5 text-[9px] font-bold uppercase",
              a.side === "long" ? "bg-emerald-900/50 text-emerald-300" : "bg-red-900/50 text-red-300",
            )}>
              {a.side}
            </span>
          )}
          {a.rvol != null && (
            <span className="text-[10px] tabular-nums text-muted-foreground">
              RVOL {a.rvol.toFixed(1)}×
            </span>
          )}
          <span
            className={cn(
              "rounded px-1.5 py-0.5 text-[10px] font-bold uppercase tracking-wide",
              styles.badge,
            )}
          >
            {PRIORITY_LABELS[a.priority] ?? `P${a.priority}`}
          </span>
        </div>
      </div>

      {/* Strategy name */}
      <div className="mt-0.5 text-[11px] text-muted-foreground truncate">
        {a.strategy_name}
      </div>

      {/* Price / volume row */}
      <div className="mt-0.5 flex flex-wrap items-center gap-x-2 gap-y-0.5 text-[11px]">
        {a.price != null && (
          <span className="tabular-nums text-foreground font-medium">
            ${fmt(a.price)}
          </span>
        )}
        {a.volume != null && (
          <span className="tabular-nums text-muted-foreground">
            vol {fmtVol(a.volume)}
          </span>
        )}
        {a.change_day_pct != null && (
          <span
            className={cn(
              "tabular-nums",
              a.change_day_pct >= 0 ? "text-emerald-400" : "text-red-400",
            )}
          >
            {a.change_day_pct >= 0 ? "+" : ""}
            {fmt(a.change_day_pct, 1)}%
          </span>
        )}
        {fmtFloat(a.float_shares) && (
          <span className="text-muted-foreground">{fmtFloat(a.float_shares)}</span>
        )}
      </div>

      {/* Reason */}
      <div className="mt-1 text-[10px] leading-tight text-muted-foreground/80 line-clamp-2">
        {a.reason}
      </div>

      {/* Timestamp */}
      <div className="mt-0.5 text-[10px] text-muted-foreground/50 text-right">
        {relTime(a.timestamp)}
      </div>
    </li>
  );
}

// ─── Scanner alerts list ──────────────────────────────────────────────────────

interface ScannerAlertsProps {
  alerts: AlertSignal[];
}

export function ScannerAlerts({ alerts }: ScannerAlertsProps) {
  const selectedTicker = useUiStore((s) => s.selectedTicker);
  const setSelectedTicker = useUiStore((s) => s.setSelectedTicker);
  const placeAlert = useLayoutStore((s) => s.placeAlert);

  // Clicking a row selects the ticker AND opens it in a free zone (first empty
  // zone, or a new tab if all are full — placeAlert handles that, and is a no-op
  // when the symbol is already on screen in this session).
  function open(a: AlertSignal) {
    setSelectedTicker(a.symbol);
    placeAlert(a);
  }

  if (alerts.length === 0) {
    return (
      <p className="p-4 text-xs text-muted-foreground">
        Scanner actif — en attente d'alertes.
      </p>
    );
  }

  return (
    <ul className="divide-y divide-border">
      {alerts.map((a) => (
        <AlertRow
          key={a.alert_id}
          alert={a}
          selected={selectedTicker === a.symbol}
          onOpen={open}
        />
      ))}
    </ul>
  );
}
