import {
  Activity,
  Bug,
  CircleDot,
  History,
  Moon,
  Orbit,
  Settings,
  Sun,
  Sunrise,
} from "lucide-react";
import { cn } from "@/lib/utils";
import { useUiStore } from "@/stores/uiStore";
import { useAppStatus } from "@/queries/useAppStatus";
import { SettingsModal } from "@/components/SettingsModal";
import { StartupModal } from "@/components/StartupModal";
import { BugReportModal } from "@/components/BugReportModal";
import type { Session } from "@/types";

const TABS: { id: Session; label: string; icon: typeof Sun }[] = [
  { id: "premarket", label: "Premarket", icon: Sunrise },
  { id: "pre_open",  label: "Pre-open",  icon: CircleDot },
  { id: "open",      label: "Open",      icon: Sun },
];

export function LeftRail() {
  const active     = useUiStore((s) => s.activeSession);
  const setActive  = useUiStore((s) => s.setActiveSession);
  const activeView    = useUiStore((s) => s.activeView);
  const setActiveView = useUiStore((s) => s.setActiveView);
  const toggleReplay = useUiStore((s) => s.toggleReplay);
  const showModal  = useUiStore((s) => s.showModal);
  const openModal  = useUiStore((s) => s.openModal);
  const closeModal = useUiStore((s) => s.closeModal);

  const status       = useAppStatus();
  const latency      = status.data?.latency;
  const latencyColor =
    latency?.level === "normal"   ? "text-emerald-400"
    : latency?.level === "warning" ? "text-amber-400"
    : latency?.level === "slow"    ? "text-orange-400"
    : "text-red-500";

  return (
    <>
      <aside className="flex h-full w-14 flex-col items-center justify-between border-r border-border bg-card py-3">
        {/* Tabs: Dashboard (Moon, first) + trading sessions */}
        <div className="flex flex-col items-center gap-2">
          {/* Dashboard — standalone KPI moodboard (no sidebar). */}
          <button
            onClick={() => setActiveView("dashboard")}
            title="Dashboard"
            className={cn(
              "flex h-10 w-10 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-accent hover:text-foreground",
              activeView === "dashboard" && "bg-accent text-foreground"
            )}
          >
            <Moon className="h-5 w-5" />
          </button>

          {/* TradeTally — embedded web app (no sidebar). */}
          <button
            onClick={() => setActiveView("tradetally")}
            title="TradeTally"
            className={cn(
              "flex h-10 w-10 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-accent hover:text-foreground",
              activeView === "tradetally" && "bg-accent text-foreground"
            )}
          >
            <Orbit className="h-5 w-5" />
          </button>

          {/* Separator between the standalone views and the trading sessions. */}
          <div className="my-1 h-px w-6 bg-border" />

          {TABS.map((tab) => {
            const Icon     = tab.icon;
            const isActive = activeView === "trading" && active === tab.id;
            return (
              <button
                key={tab.id}
                onClick={() => {
                  setActive(tab.id);
                  setActiveView("trading");
                }}
                title={tab.label}
                className={cn(
                  "flex h-10 w-10 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-accent hover:text-foreground",
                  isActive && "bg-accent text-foreground"
                )}
              >
                <Icon className="h-5 w-5" />
              </button>
            );
          })}
        </div>

        {/* Bottom: quick actions + latency */}
        <div className="flex flex-col items-center gap-3">
          {/* Quick actions — Market Replay, Settings, Bug report — stacked above
              the latency readout. Everything else used to live behind the ⋮ menu
              here now lives inside Settings. */}
          <div className="flex flex-col items-center gap-2">
            <button
              onClick={toggleReplay}
              title="Market Replay"
              className="flex h-10 w-10 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
            >
              <History className="h-5 w-5" />
            </button>
            <button
              onClick={() => showModal("settings")}
              title="Settings"
              className="flex h-10 w-10 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
            >
              <Settings className="h-5 w-5" />
            </button>
            <button
              onClick={() => showModal("bug-report")}
              title="Signaler un bug"
              className="flex h-10 w-10 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
            >
              <Bug className="h-5 w-5" />
            </button>
          </div>

          <div
            className={cn("flex flex-col items-center text-[10px]", latencyColor)}
            title="websocket_to_ui_latency_ms"
          >
            <Activity className="h-4 w-4" />
            <span className="mt-0.5 tabular-nums">
              {latency ? `${latency.websocket_to_ui_ms}ms` : "—"}
            </span>
          </div>
        </div>
      </aside>

      {/* Modals */}
      <StartupModal
        open={openModal === "startup"}
        onClose={closeModal}
      />
      <SettingsModal
        open={openModal === "settings"}
        onClose={closeModal}
      />
      <BugReportModal
        open={openModal === "bug-report"}
        onClose={closeModal}
      />
    </>
  );
}
