import { useUiStore } from "@/stores/uiStore";
import { useLayoutStore } from "@/stores/layoutStore";
import { cn } from "@/lib/utils";
import { ChartZone } from "@/components/ChartZone";

export function MainWindow() {
  const activeSession = useUiStore((s) => s.activeSession);
  const tabs          = useLayoutStore((s) => s.tabs[activeSession]);
  const activeTabId   = useLayoutStore((s) => s.activeTabId[activeSession]);

  return (
    <main className="flex flex-1 flex-col overflow-hidden">
      {/* Single chart per session — switching ticker happens from the alert
          sidebar, not via tabs. Each session's sole tab stays mounted (inactive
          ones hidden with display:none) so its ChartZone — and the underlying
          lightweight-charts instance — survives a session switch, preserving the
          user's pan/zoom and the SL/TP/entry price lines. */}
      {tabs.map((tab) => (
        <div
          key={tab.tab_id}
          className={cn(
            "grid flex-1 grid-cols-1 grid-rows-1 gap-2 p-2 min-h-0",
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
