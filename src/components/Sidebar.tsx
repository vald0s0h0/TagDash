import { useEffect, useRef } from "react";
import { AlarmClock, Bell, BriefcaseBusiness, ListOrdered, Radar } from "lucide-react";
import { useQuery } from "@tanstack/react-query";
import { useUiStore } from "@/stores/uiStore";
import { useActiveAlerts, useScreenerMatches, useStartScanner } from "@/queries/useScanner";
import { useLayoutStore } from "@/stores/layoutStore";
import { useAlertStatusStore } from "@/stores/alertStatusStore";
import { ScannerAlerts } from "@/components/ScannerAlerts";
import { ScreenerPanel } from "@/components/ScreenerPanel";
import { PositionsPanel } from "@/components/PositionsPanel";
import { OrdersPanel } from "@/components/OrdersPanel";
import { AlarmsPanel } from "@/components/AlarmsPanel";
import { api } from "@/lib/tauri";
import type { AlertSignal, Session } from "@/types";

function SectionHeader({
  icon: Icon,
  title,
  count,
}: {
  icon: typeof Bell;
  title: string;
  count?: number;
}) {
  return (
    <div className="flex items-center justify-between border-b border-border px-3 py-2 text-xs uppercase tracking-wider text-muted-foreground">
      <div className="flex items-center gap-2">
        <Icon className="h-3.5 w-3.5" />
        <span>{title}</span>
      </div>
      {typeof count === "number" && (
        <span className="rounded bg-accent px-1.5 py-0.5 text-[10px] text-foreground tabular-nums">
          {count}
        </span>
      )}
    </div>
  );
}

export function Sidebar() {
  const active       = useUiStore((s) => s.activeSession);
  const startScanner = useStartScanner();
  const alertsQuery  = useActiveAlerts();
  const alerts       = alertsQuery.data ?? [];
  const placeAlert   = useLayoutStore((s) => s.placeAlert);
  const setSelectedTicker = useUiStore((s) => s.setSelectedTicker);
  // Symbol shown in the active session's (single) chart — drives the release →
  // open-next behaviour below.
  const activeZoneSymbol = useLayoutStore(
    (s) => s.tabs[active][0]?.zones[0]?.symbol ?? null,
  );

  const isPreOpen     = active === "pre_open";
  const screenerQuery = useScreenerMatches();
  const screener      = screenerQuery.data ?? [];

  const { data: positions = [] } = useQuery({
    queryKey: ["internal_positions"],
    queryFn:  () => api.getInternalPositions(),
    refetchInterval: 1000,
  });
  const { data: orders = [] } = useQuery({
    queryKey: ["internal_orders"],
    queryFn:  () => api.getInternalOrders(),
    refetchInterval: 1000,
  });
  const { data: alarms = [] } = useQuery({
    queryKey: ["all_alarms"],
    queryFn:  () => api.getAllAlarms(),
    refetchInterval: 2000,
  });
  // One badge per ticker, armed alarms only (matches AlarmsPanel's condensing).
  const alarmCount = new Set(
    alarms.filter((a) => a.triggered_at == null).map((a) => a.symbol)
  ).size;

  // Track which alert_ids have already been fed to the layout store
  // so we don't re-trigger auto-placement on every 800ms poll.
  const placedRef = useRef<Set<string>>(new Set());

  // Start the scanner once on mount
  useEffect(() => {
    startScanner.mutate();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Auto-place new alerts into zones as they arrive
  useEffect(() => {
    for (const alert of alerts as AlertSignal[]) {
      if (!placedRef.current.has(alert.alert_id)) {
        placedRef.current.add(alert.alert_id);
        placeAlert(alert);
      }
    }
  }, [alerts, placeAlert]);

  // On release: when the active session's chart goes from a ticker to empty (a
  // non-null → null transition of the SAME session's zone = a Libérer), land on
  // the most recent still-pending (not released) alert of that session — so the
  // chart never sits empty while other tickers wait.
  const prevZoneRef = useRef<{ session: Session; symbol: string | null }>({
    session: active,
    symbol:  activeZoneSymbol,
  });
  useEffect(() => {
    const prev = prevZoneRef.current;
    prevZoneRef.current = { session: active, symbol: activeZoneSymbol };
    if (prev.session !== active || prev.symbol == null || activeZoneSymbol != null) return;
    const released = useAlertStatusStore.getState().released;
    const next = alerts
      .filter((a) => a.session === active && !released.has(a.symbol))
      .sort((x, y) => new Date(y.timestamp).getTime() - new Date(x.timestamp).getTime())[0];
    if (next) {
      setSelectedTicker(next.symbol);
      placeAlert(next);
    }
  }, [activeZoneSymbol, active, alerts, placeAlert, setSelectedTicker]);

  // Filter displayed alerts by currently active session tab
  const sessionAlerts = alerts.filter((a) => a.session === active);

  // Clicking a ticker in Positions / Orders opens the matching scanner alert
  // (same behaviour as clicking the card in the scanner list).
  function handleTickerClick(symbol: string) {
    setSelectedTicker(symbol);
    const match = alerts.find((a) => a.symbol === symbol && a.session === active)
                ?? alerts.find((a) => a.symbol === symbol);
    if (match) {
      placeAlert(match);
    }
  }

  return (
    <aside className="flex h-full w-72 flex-col border-r border-border bg-background">
      {/* Top section: live screener (pre-open) vs alert feed (premarket / open).
          The screener is "what's happening now" — tickers come and go in real
          time. The alert feed is "what just happened" — events accumulate. */}
      {isPreOpen ? (
        <div className="flex flex-1 flex-col overflow-hidden">
          <SectionHeader
            icon={Radar}
            title="Screener · pre-open"
            count={screener.length}
          />
          <div className="flex-1 overflow-y-auto">
            <ScreenerPanel matches={screener} />
          </div>
        </div>
      ) : (
        <div className="flex flex-1 flex-col overflow-hidden">
          <SectionHeader
            icon={Bell}
            title={`Alerts · ${active}`}
            count={sessionAlerts.length}
          />
          <div className="flex-1 overflow-y-auto">
            <ScannerAlerts alerts={sessionAlerts} />
          </div>
        </div>
      )}

      {/* Alarmes — condensed watchlist of armed price levels. Click a ticker to
          open its chart in the Open tab using the strategy's criteria. */}
      <div className="flex flex-col overflow-hidden border-t border-border" style={{ minHeight: "4rem", maxHeight: "10rem" }}>
        <SectionHeader icon={AlarmClock} title="Alarmes" count={alarmCount} />
        <div className="flex-1 overflow-y-auto">
          <AlarmsPanel />
        </div>
      </div>

      {/* Positions */}
      <div className="flex flex-col overflow-hidden border-t border-border" style={{ minHeight: "5rem", maxHeight: "12rem" }}>
        <SectionHeader icon={BriefcaseBusiness} title="Positions" count={positions.length} />
        <div className="flex-1 overflow-y-auto">
          <PositionsPanel onTickerClick={handleTickerClick} />
        </div>
      </div>

      {/* Pending orders */}
      <div className="flex flex-col overflow-hidden border-t border-border" style={{ minHeight: "5rem", maxHeight: "12rem" }}>
        <SectionHeader icon={ListOrdered} title="Ordres" count={orders.length} />
        <div className="flex-1 overflow-y-auto">
          <OrdersPanel onTickerClick={handleTickerClick} />
        </div>
      </div>
    </aside>
  );
}
