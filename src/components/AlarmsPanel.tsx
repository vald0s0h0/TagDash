import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Trash2 } from "lucide-react";
import type { AlarmView, AlertSignal } from "@/types";
import { useUiStore } from "@/stores/uiStore";
import { useLayoutStore } from "@/stores/layoutStore";
import { api } from "@/lib/tauri";
import { cn } from "@/lib/utils";

// Mirror ChartZone's priority palette so the badge reads the same everywhere.
const PRIORITY_BADGE: Record<number, string> = {
  1: "bg-zinc-700 text-zinc-300",
  2: "bg-blue-900/60 text-blue-300",
  3: "bg-amber-900/60 text-amber-300",
  4: "bg-orange-900/60 text-orange-300",
  5: "bg-red-900/70 text-red-300",
};

/** Build a synthetic AlertSignal so clicking an alarm opens its chart in the
 *  Open tab using the strategy's card (panes / indicators / info band) — exactly
 *  like a real alert placement. session "open" routes openInActiveZone there. */
function alarmToAlert(a: AlarmView): AlertSignal {
  return {
    alert_id:       `alarm-open-${a.symbol}-${a.strategy_id ?? "none"}`,
    timestamp:      new Date().toISOString(),
    symbol:         a.symbol,
    strategy_id:    a.strategy_id ?? "",
    strategy_name:  a.strategy_name,
    priority:       a.priority,
    session:        "open",
    price:          a.price,
    bid:            null,
    ask:            null,
    spread:         null,
    volume:         null,
    rvol:           null,
    change_day_pct: null,
    float_shares:   null,
    news_today:     false,
    halted:         null,
    latency_ui_ms:  null,
    reason:         `Alarme · ${a.strategy_name}`,
    display_timeframe: null,
    side:           null,
  };
}

/** Keep one row per ticker (highest priority wins), armed alarms only. */
function condense(alarms: AlarmView[]): AlarmView[] {
  const bySymbol = new Map<string, AlarmView>();
  for (const a of alarms) {
    if (a.triggered_at != null) continue;
    const existing = bySymbol.get(a.symbol);
    if (!existing || a.priority > existing.priority) bySymbol.set(a.symbol, a);
  }
  return [...bySymbol.values()].sort((x, y) => y.priority - x.priority);
}

export function AlarmsPanel() {
  const qc                = useQueryClient();
  const setSelectedTicker = useUiStore((s) => s.setSelectedTicker);
  const openInActiveZone  = useLayoutStore((s) => s.openInActiveZone);

  const { data: alarms = [] } = useQuery({
    queryKey: ["all_alarms"],
    queryFn:  () => api.getAllAlarms(),
    refetchInterval: 2000,
  });

  const rows = condense(alarms);

  if (rows.length === 0) {
    return (
      <p className="px-3 py-2 text-xs text-muted-foreground/60">
        Aucune alarme active.
      </p>
    );
  }

  function open(a: AlarmView) {
    setSelectedTicker(a.symbol);
    openInActiveZone(alarmToAlert(a));
  }

  // Each row is condensed per-ticker, so the trash removes every armed alarm on
  // that symbol (the ticker leaves the watchlist entirely).
  async function remove(symbol: string) {
    const ids = alarms.filter((a) => a.symbol === symbol).map((a) => a.id);
    await Promise.all(ids.map((id) => api.deleteAlarm(id).catch(() => {})));
    qc.invalidateQueries({ queryKey: ["all_alarms"] });
  }

  return (
    <div className="flex flex-col">
      {rows.map((a) => (
        <div
          key={a.symbol}
          className="group flex items-center gap-2 border-b border-border/40 px-3 py-1.5 text-xs last:border-none hover:bg-accent/30"
        >
          <button
            onClick={() => open(a)}
            title={`${a.symbol} — ${a.strategy_name} · ouvrir dans Open`}
            className="flex flex-1 items-center gap-2 text-left"
          >
            <span className={cn(
              "shrink-0 rounded px-1 py-0.5 text-[9px] font-bold uppercase",
              PRIORITY_BADGE[a.priority] ?? PRIORITY_BADGE[1],
            )}>
              P{a.priority}
            </span>
            <span className="font-semibold tabular-nums">{a.symbol}</span>
          </button>
          <button
            onClick={() => remove(a.symbol)}
            title="Supprimer l'alarme"
            className="shrink-0 rounded p-0.5 text-muted-foreground/40 opacity-0 transition-opacity hover:bg-red-900/30 hover:text-red-400 group-hover:opacity-100"
          >
            <Trash2 className="h-3.5 w-3.5" />
          </button>
        </div>
      ))}
    </div>
  );
}
