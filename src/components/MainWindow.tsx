import { useUiStore } from "@/stores/uiStore";
import { useLayoutStore } from "@/stores/layoutStore";
import { cn } from "@/lib/utils";
import { ChartZone } from "@/components/ChartZone";
import { SessionClock } from "@/components/SessionClock";
import type { Session } from "@/types";

export function MainWindow() {
  const activeSession = useUiStore((s) => s.activeSession);
  const tabs          = useLayoutStore((s) => s.tabs[activeSession]);
  const activeTabId   = useLayoutStore((s) => s.activeTabId[activeSession]);
  const setActiveTab  = useLayoutStore((s) => s.setActiveTab);

  const activeTab   = tabs.find((t) => t.tab_id === activeTabId) ?? tabs[0];
  const isOpen      = activeSession === "open";
  const multiTab    = tabs.length > 1;

  // Count occupied zones in a tab for the badge
  function occupiedCount(tab_id: string) {
    const t = tabs.find((x) => x.tab_id === tab_id);
    return t ? t.zones.filter((z) => z.symbol !== null).length : 0;
  }

  return (
    <main className="flex flex-1 flex-col overflow-hidden">
      {/* Dynamic tab bar — shown only when >1 tab for this session */}
      {multiTab && (
        <div className="flex items-center gap-0.5 border-b border-border bg-card px-2 py-1">
          {tabs.map((tab) => {
            const occ     = occupiedCount(tab.tab_id);
            const isActive = tab.tab_id === activeTabId;
            return (
              <button
                key={tab.tab_id}
                onClick={() => setActiveTab(activeSession as Session, tab.tab_id)}
                className={cn(
                  "flex items-center gap-1.5 rounded px-2.5 py-1 text-xs transition-colors",
                  isActive
                    ? "bg-accent font-medium text-foreground"
                    : "text-muted-foreground hover:bg-accent/50 hover:text-foreground"
                )}
              >
                {tab.label}
                {occ > 0 && (
                  <span
                    className={cn(
                      "inline-flex h-4 w-4 items-center justify-center rounded-full text-[9px] tabular-nums",
                      isActive
                        ? "bg-primary/30 text-primary"
                        : "bg-muted text-muted-foreground"
                    )}
                  >
                    {occ}
                  </span>
                )}
              </button>
            );
          })}
          <SessionClock />
        </div>
      )}

      {/* Single-tab header (shown when only one tab) */}
      {!multiTab && (
        <div className="flex items-center gap-2 border-b border-border bg-card px-4 py-2 text-xs uppercase tracking-wider text-muted-foreground">
          <span className="font-semibold text-foreground">
            {activeTab?.label ?? activeSession}
          </span>
          <span className="text-[10px]">
            ·{" "}
            {isOpen ? "4 zones" : "1 zone"}
          </span>
          <SessionClock />
        </div>
      )}

      {/* Zone grids — every tab of the session stays mounted; inactive ones are
          hidden with display:none. This keeps each ChartZone (and its underlying
          lightweight-charts instance) alive across tab switches, so the user's
          pan/zoom and the SL/TP/entry price lines are preserved instead of being
          recreated from scratch on every switch. */}
      {tabs.map((tab) => (
        <div
          key={tab.tab_id}
          className={cn(
            "grid flex-1 gap-2 p-2 min-h-0",
            isOpen
              ? "grid-cols-2 grid-rows-2"
              : "grid-cols-1 grid-rows-1",
            tab.tab_id !== activeTabId && "hidden"
          )}
        >
          {tab.zones.map((zone) => (
            <ChartZone key={zone.zone_id} zone={zone} />
          ))}
        </div>
      ))}
    </main>
  );
}
