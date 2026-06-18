import { useEffect, useState } from "react";
import { Bird } from "lucide-react";
import { useQuery } from "@tanstack/react-query";
import type { AlertSignal, Position } from "@/types";
import { useUiStore } from "@/stores/uiStore";
import { useLayoutStore } from "@/stores/layoutStore";
import { useAlertStatusStore } from "@/stores/alertStatusStore";
import { useChartStore } from "@/stores/chartStore";
import { api } from "@/lib/tauri";
import { cn } from "@/lib/utils";

// ─── Priority badge colours (the P1…P5 chip; the left state-bar is separate) ────

const PRIORITY_BADGE: Record<number, string> = {
  1: "bg-zinc-700 text-zinc-300",
  2: "bg-blue-900/60 text-blue-300",
  3: "bg-amber-900/60 text-amber-300",
  4: "bg-orange-900/60 text-orange-300",
  5: "bg-red-900/70 text-red-300 animate-pulse",
};

const PRIORITY_LABELS: Record<number, string> = {
  1: "P1",
  2: "P2",
  3: "P3",
  4: "P4",
  5: "P5 !!",
};

// ─── Card state vs the chart ────────────────────────────────────────────────────

type RowState = "active" | "waiting" | "observed" | "released";

/** How long after an alert appears its card blinks ("en attente"). */
const WAITING_MS = 10_000;

// Resting shades reuse the two pre-existing intensities (do NOT change their
// values): /60 = high, /40 = low. Hover adds a third, even lower wash (/20).
// A released card has no resting shade at all.
const STATE_SHADE: Record<RowState, string> = {
  active:   "bg-accent/60", // high — currently shown in the chart
  waiting:  "bg-accent/40", // low  — just appeared (blinks)
  observed: "bg-accent/40", // low  — seen, not released
  released: "",             // none — released (Libérer)
};

const STATE_BAR: Record<RowState, string> = {
  active:   "bg-red-500",  // red       — currently shown in the chart
  waiting:  "bg-red-500",  // red       — blinks (just appeared)
  observed: "bg-zinc-600", // grey      — seen, not released
  released: "bg-zinc-800", // dark grey — released (Libérer)
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
  state: RowState;
  position: Position | null;
  onOpen: (alert: AlertSignal) => void;
  onRelease: (symbol: string) => void;
}

function AlertRow({ alert: a, state, position, onOpen, onRelease }: AlertRowProps) {
  const blink = state === "waiting";

  return (
    <li
      className="group relative cursor-pointer"
      onClick={() => onOpen(a)}
    >
      {/* Base state shade (reuses the two existing intensities; blinks when
          waiting). pointer-events-none so the whole card stays clickable. */}
      <span
        aria-hidden
        className={cn(
          "pointer-events-none absolute inset-0",
          STATE_SHADE[state],
          blink && "animate-pulse",
        )}
      />
      {/* Hover shade — a third, even lower wash, additive on top of the base. */}
      <span
        aria-hidden
        className="pointer-events-none absolute inset-0 bg-accent/20 opacity-0 transition-opacity group-hover:opacity-100"
      />
      {/* Left state-bar — red (active) / blinking red (waiting) / grey (idle). */}
      <span
        aria-hidden
        className={cn(
          "absolute inset-y-0 left-0 z-10 w-0.5",
          STATE_BAR[state],
          blink && "animate-pulse",
        )}
      />

      <div className="relative z-10 px-3 py-2 pl-4">
        {/* Row header */}
        <div className="flex items-center justify-between gap-1">
          {position ? (
            // Open position → the ticker name becomes a P&L-coloured badge
            // (green = winning, red = losing); back to plain text when flat.
            <span
              title={`Position ${position.side} · PnL ${fmt(position.unrealized_pnl)}`}
              className={cn(
                "rounded px-1.5 py-0.5 text-sm font-bold tabular-nums",
                (position.unrealized_pnl ?? 0) >= 0
                  ? "bg-emerald-600/30 text-emerald-200"
                  : "bg-red-600/30 text-red-200",
              )}
            >
              {a.symbol}
            </span>
          ) : (
            <span className="text-sm font-semibold tabular-nums">{a.symbol}</span>
          )}
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
                PRIORITY_BADGE[a.priority] ?? PRIORITY_BADGE[1],
              )}
            >
              {PRIORITY_LABELS[a.priority] ?? `P${a.priority}`}
            </span>
            {/* Release (bird) — inline with the badges, revealed on hover. */}
            <button
              title="Libérer"
              onClick={(e) => { e.stopPropagation(); onRelease(a.symbol); }}
              className="rounded p-0.5 text-muted-foreground/40 opacity-0 transition-opacity hover:bg-accent hover:text-rose-300 group-hover:opacity-100"
            >
              <Bird className="h-3.5 w-3.5" />
            </button>
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
      </div>
    </li>
  );
}

// ─── Scanner alerts list ──────────────────────────────────────────────────────

interface ScannerAlertsProps {
  alerts: AlertSignal[];
}

export function ScannerAlerts({ alerts }: ScannerAlertsProps) {
  const setSelectedTicker = useUiStore((s) => s.setSelectedTicker);
  const activeSession     = useUiStore((s) => s.activeSession);
  const placeAlert        = useLayoutStore((s) => s.placeAlert);
  const releaseZone       = useLayoutStore((s) => s.releaseZone);
  const markReleased      = useAlertStatusStore((s) => s.markReleased);
  // The symbol currently displayed in this session's (single) chart.
  const activeSymbol = useLayoutStore(
    (s) => s.tabs[activeSession][0]?.zones[0]?.symbol ?? null,
  );
  const released = useAlertStatusStore((s) => s.released);

  // Open positions (deduped with the sidebar/chart by React-Query key) → P&L
  // colours the ticker badge.
  const { data: positions = [] } = useQuery({
    queryKey: ["internal_positions"],
    queryFn:  () => api.getInternalPositions(),
    refetchInterval: 1000,
  });

  // 1s tick so a freshly-arrived alert's blinking "waiting" card settles after
  // WAITING_MS even when no new poll re-renders the list.
  const [, force] = useState(0);
  useEffect(() => {
    const id = setInterval(() => force((n) => n + 1), 1000);
    return () => clearInterval(id);
  }, []);

  // Clicking a row shows it in the session's chart (placeAlert replaces the single
  // zone; no-op when already shown).
  function open(a: AlertSignal) {
    setSelectedTicker(a.symbol);
    placeAlert(a);
  }

  // Bird → release. If the ticker is the one on the chart, free the zone (which
  // also triggers the "open most recent pending" behaviour in the Sidebar);
  // otherwise just flag it released (greys the card).
  function release(symbol: string) {
    const zone = useLayoutStore.getState().tabs[activeSession][0]?.zones[0];
    if (zone && zone.symbol === symbol) {
      useChartStore.getState().clearZone(zone.zone_id);
      releaseZone(zone.zone_id);
      api.clearZoneContext(zone.zone_id).catch(() => {});
    } else {
      markReleased(symbol);
    }
  }

  function rowState(a: AlertSignal): RowState {
    if (a.symbol === activeSymbol) return "active";
    if (released.has(a.symbol)) return "released"; // dark grey, no shade, never blinks
    if (Date.now() - new Date(a.timestamp).getTime() < WAITING_MS) return "waiting";
    return "observed";
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
          state={rowState(a)}
          position={positions.find((p) => p.symbol === a.symbol) ?? null}
          onOpen={open}
          onRelease={release}
        />
      ))}
    </ul>
  );
}
