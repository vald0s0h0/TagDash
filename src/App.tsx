import { useEffect, useMemo } from "react";
import { TitleBar } from "@/components/TitleBar";
import { LeftRail } from "@/components/LeftRail";
import { Sidebar } from "@/components/Sidebar";
import { MainWindow } from "@/components/MainWindow";
import { LogsPanel } from "@/components/LogsPanel";
import { ReplayToolbar } from "@/components/ReplayToolbar";
import { TickerSpotlight } from "@/components/TickerSpotlight";
import { Dashboard } from "@/components/dashboard/Dashboard";
import { useUiStore } from "@/stores/uiStore";
import { useLayoutStore } from "@/stores/layoutStore";
import { useAlertNotifications } from "@/queries/useAlertNotifications";
import { useHotkeys } from "@/hooks/useHotkeys";
import { api } from "@/lib/tauri";

export default function App() {
  const logsOpen = useUiStore((s) => s.logsOpen);
  const activeView = useUiStore((s) => s.activeView);
  const setDismissedScreener = useUiStore((s) => s.setDismissedScreener);

  // App-level OS notifications on every new scanner alert (opt-in in Settings).
  useAlertNotifications();

  // Global chart hotkeys (keyboard chords / extra mouse buttons → the zone under
  // the cursor). User-configured in Settings → Hotkeys.
  useHotkeys();

  // Hydrate today's pre-open screener dismissals from the DB so cards the user
  // removed stay hidden across restarts (until the next trading day).
  useEffect(() => {
    api.getScreenerDismissals().then(setDismissedScreener).catch(() => {});
  }, [setDismissedScreener]);

  // Tell the backend which symbols are displayed — the union of every assigned
  // zone across ALL tabs and sessions (not just the active tab). The live feed
  // tick-streams these (trades+quotes) on top of the broad wildcard surveillance
  // tier so visible charts update tick-by-tick. Taking the full union means
  // switching tabs never tears down a subscription (no data gap); a symbol is
  // only dropped when its zone is released. There is no "universe" toggle
  // anymore: the broad tier always covers the whole US market via `*`.
  const tabs     = useLayoutStore((s) => s.tabs);
  const focusKey = useMemo(() => {
    const syms = new Set<string>();
    for (const tabList of Object.values(tabs)) {
      for (const tab of tabList) {
        for (const z of tab.zones) {
          if (z.symbol) syms.add(z.symbol);
        }
      }
    }
    return [...syms].sort().join(",");
  }, [tabs]);

  useEffect(() => {
    const symbols = focusKey ? focusKey.split(",") : [];
    api.setFocusSymbols(symbols).catch(() => {});
  }, [focusKey]);

  return (
    <div className="flex h-full w-full flex-col overflow-hidden">
      {/* Custom OS title bar (native decorations disabled): logo · strategy
          toggles · NY clock · window controls. */}
      <TitleBar />
      <div className="flex flex-1 overflow-hidden">
        <LeftRail />
        {activeView === "dashboard" ? (
          // KPI moodboard — full-bleed, no sidebar.
          <Dashboard />
        ) : (
          <>
            <Sidebar />
            <div className="flex flex-1 flex-col overflow-hidden">
              {/* Market Replay transport bar — rendered only when activated (menu). */}
              <ReplayToolbar />
              <MainWindow />
              {logsOpen && <LogsPanel />}
            </div>
          </>
        )}
        <TickerSpotlight />
      </div>
    </div>
  );
}
