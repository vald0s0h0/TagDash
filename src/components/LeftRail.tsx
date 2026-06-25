import {
  Activity,
  Bug,
  CircleDot,
  Database,
  DownloadCloud,
  History,
  Mic,
  Moon,
  MoreVertical,
  Newspaper,
  Orbit,
  Play,
  Radio,
  RefreshCw,
  ScrollText,
  Settings,
  Sun,
  Sunrise,
  Table2,
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
import { TickersTableModal } from "@/components/TickersTableModal";
import { FlatFilesModal } from "@/components/FlatFilesModal";
import { SttModal } from "@/components/SttModal";
import { UpdateModal } from "@/components/UpdateModal";
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

        {/* Bottom: quick actions + latency + menu */}
        <div className="flex flex-col items-center gap-3">
          {/* Quick actions — Market Replay, Settings, Bug report (same order &
              icons as the old ⋮ menu), stacked above the latency readout. */}
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
              <DropdownMenuItem onClick={() => showModal("tickers-table")}>
                <Table2 className="mr-2 h-4 w-4" />
                Données tickers (DB)
              </DropdownMenuItem>
              <DropdownMenuItem onClick={() => showModal("flat-files")}>
                <Database className="mr-2 h-4 w-4" />
                Gestion Flat Files
              </DropdownMenuItem>
              <DropdownMenuSeparator />
              <DropdownMenuItem onClick={toggleLogs}>
                <ScrollText className="mr-2 h-4 w-4" />
                Logs
              </DropdownMenuItem>
              <DropdownMenuItem onClick={() => showModal("stt")}>
                <Mic className="mr-2 h-4 w-4" />
                Dictée vocale (micro &amp; file)
              </DropdownMenuItem>
              <DropdownMenuSeparator />
              <DropdownMenuItem onClick={() => showModal("sync-status")}>
                <RefreshCw className="mr-2 h-4 w-4" />
                Sync TradeTally Status
              </DropdownMenuItem>
              <DropdownMenuSeparator />
              <DropdownMenuItem onClick={() => showModal("update")}>
                <DownloadCloud className="mr-2 h-4 w-4" />
                Mise à jour
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
      <TickersTableModal
        open={openModal === "tickers-table"}
        onClose={closeModal}
      />
      <FlatFilesModal
        open={openModal === "flat-files"}
        onClose={closeModal}
      />
      <SttModal
        open={openModal === "stt"}
        onClose={closeModal}
      />
      <UpdateModal
        open={openModal === "update"}
        onClose={closeModal}
      />
    </>
  );
}
