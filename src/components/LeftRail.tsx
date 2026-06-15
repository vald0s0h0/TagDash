import {
  Activity,
  Bug,
  CircleDot,
  History,
  MoreVertical,
  Newspaper,
  Play,
  Radio,
  RefreshCw,
  ScrollText,
  Settings,
  Sun,
  Sunrise,
} from "lucide-react";
import { cn } from "@/lib/utils";
import { useUiStore } from "@/stores/uiStore";
import { useAppStatus } from "@/queries/useAppStatus";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { SettingsModal } from "@/components/SettingsModal";
import { SyncStatusModal } from "@/components/SyncStatusModal";
import { StartupModal } from "@/components/StartupModal";
import { FeedDiagnosticsModal } from "@/components/FeedDiagnosticsModal";
import { NewsDebugModal } from "@/components/NewsDebugModal";
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
  const toggleLogs = useUiStore((s) => s.toggleLogs);
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
        {/* Session tabs */}
        <div className="flex flex-col items-center gap-2">
          {TABS.map((tab) => {
            const Icon     = tab.icon;
            const isActive = active === tab.id;
            return (
              <button
                key={tab.id}
                onClick={() => setActive(tab.id)}
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

        {/* Bottom: latency + menu */}
        <div className="flex flex-col items-center gap-3">
          <div
            className={cn("flex flex-col items-center text-[10px]", latencyColor)}
            title="websocket_to_ui_latency_ms"
          >
            <Activity className="h-4 w-4" />
            <span className="mt-0.5 tabular-nums">
              {latency ? `${latency.websocket_to_ui_ms}ms` : "—"}
            </span>
          </div>

          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <button
                title="More"
                className="flex h-10 w-10 items-center justify-center rounded-md text-muted-foreground hover:bg-accent hover:text-foreground"
              >
                <MoreVertical className="h-5 w-5" />
              </button>
            </DropdownMenuTrigger>
            <DropdownMenuContent side="right" align="end" className="w-52">
              <DropdownMenuItem onClick={() => showModal("startup")}>
                <Play className="mr-2 h-4 w-4" />
                Startup Pipeline
              </DropdownMenuItem>
              <DropdownMenuItem onClick={() => showModal("feed-diagnostics")}>
                <Radio className="mr-2 h-4 w-4" />
                Diagnostic flux live
              </DropdownMenuItem>
              <DropdownMenuItem onClick={() => showModal("news-debug")}>
                <Newspaper className="mr-2 h-4 w-4" />
                Debug news premarket
              </DropdownMenuItem>
              <DropdownMenuSeparator />
              <DropdownMenuItem onClick={toggleReplay}>
                <History className="mr-2 h-4 w-4" />
                Market Replay
              </DropdownMenuItem>
              <DropdownMenuSeparator />
              <DropdownMenuItem onClick={() => showModal("settings")}>
                <Settings className="mr-2 h-4 w-4" />
                Settings
              </DropdownMenuItem>
              <DropdownMenuItem onClick={toggleLogs}>
                <ScrollText className="mr-2 h-4 w-4" />
                Logs
              </DropdownMenuItem>
              <DropdownMenuSeparator />
              <DropdownMenuItem onClick={() => showModal("sync-status")}>
                <RefreshCw className="mr-2 h-4 w-4" />
                Sync TradeTally Status
              </DropdownMenuItem>
              <DropdownMenuSeparator />
              <DropdownMenuItem onClick={() => showModal("bug-report")}>
                <Bug className="mr-2 h-4 w-4" />
                Signaler un bug
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        </div>
      </aside>

      {/* Modals */}
      <StartupModal
        open={openModal === "startup"}
        onClose={closeModal}
      />
      <FeedDiagnosticsModal
        open={openModal === "feed-diagnostics"}
        onClose={closeModal}
      />
      <NewsDebugModal
        open={openModal === "news-debug"}
        onClose={closeModal}
      />
      <SettingsModal
        open={openModal === "settings"}
        onClose={closeModal}
      />
      <SyncStatusModal
        open={openModal === "sync-status"}
        onClose={closeModal}
      />
      <BugReportModal
        open={openModal === "bug-report"}
        onClose={closeModal}
      />
    </>
  );
}
