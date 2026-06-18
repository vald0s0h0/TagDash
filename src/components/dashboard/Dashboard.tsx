import { useEffect, useRef } from "react";
import { FolderOpen, RefreshCw, RotateCcw, SlidersHorizontal } from "lucide-react";
import {
  DropdownMenu,
  DropdownMenuTrigger,
  DropdownMenuContent,
  DropdownMenuCheckboxItem,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
} from "@/components/ui/dropdown-menu";
import { api } from "@/lib/tauri";
import { GridCard } from "./GridCard";
import { CARD_DEFS } from "./cards";
import { useDashboardStore, GRID_COLS, GRID_ROWS } from "@/stores/dashboardStore";
import { useDailyBackground, useDashboardTrades, useSyncTrades } from "./useDashboard";

/** Full-bleed KPI moodboard: a daily photo background + glass cards on an invisible
 *  10×6 grid. No sidebar. TradeTally is the source of truth — the trades re-sync
 *  every time this view opens. */
export function Dashboard() {
  const gridRef = useRef<HTMLDivElement>(null);

  const { data: bg } = useDailyBackground();
  const { data: trades = [] } = useDashboardTrades();
  const sync = useSyncTrades();

  const layout = useDashboardStore((s) => s.layout);
  const editing = useDashboardStore((s) => s.editing);
  const toggleEditing = useDashboardStore((s) => s.toggleEditing);
  const toggleVisible = useDashboardStore((s) => s.toggleVisible);
  const resetLayout = useDashboardStore((s) => s.resetLayout);

  // Refresh from TradeTally on every open (the tab unmounts when you leave it).
  useEffect(() => {
    sync.mutate();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return (
    <div className="relative flex-1 overflow-hidden">
      {/* Daily background — full-bleed, centred, undistorted. */}
      {bg?.data_url ? (
        <img
          src={bg.data_url}
          alt=""
          className="absolute inset-0 z-0 h-full w-full object-cover object-center"
        />
      ) : (
        <div className="absolute inset-0 z-0 bg-background" />
      )}
      {/* Contrast veil so glass cards stay legible over any photo. */}
      <div className="absolute inset-0 z-0 bg-black/40" />

      {/* Discreet controls (top-right). */}
      <div className="absolute right-3 top-3 z-20 flex items-center gap-2">
        {sync.isPending && <span className="text-[11px] text-white/70">Sync…</span>}
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <button
              title="Options du dashboard"
              className="flex h-8 w-8 items-center justify-center rounded-md bg-white/10 text-white/80 backdrop-blur transition-colors hover:bg-white/20 hover:text-white"
            >
              <SlidersHorizontal className="h-4 w-4" />
            </button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end" className="w-60">
            <DropdownMenuLabel>Cartes</DropdownMenuLabel>
            {CARD_DEFS.map((c) => (
              <DropdownMenuCheckboxItem
                key={c.id}
                checked={layout[c.id]?.visible ?? false}
                onSelect={(e) => e.preventDefault()}
                onCheckedChange={() => toggleVisible(c.id)}
              >
                {c.title}
              </DropdownMenuCheckboxItem>
            ))}

            <DropdownMenuSeparator />
            <DropdownMenuCheckboxItem
              checked={editing}
              onSelect={(e) => e.preventDefault()}
              onCheckedChange={() => toggleEditing()}
            >
              Éditer la disposition
            </DropdownMenuCheckboxItem>
            <DropdownMenuItem onClick={() => resetLayout()}>
              <RotateCcw className="mr-2 h-4 w-4" />
              Réinitialiser la disposition
            </DropdownMenuItem>

            <DropdownMenuSeparator />
            <DropdownMenuItem onClick={() => sync.mutate()}>
              <RefreshCw className="mr-2 h-4 w-4" />
              Rafraîchir les trades
            </DropdownMenuItem>
            <DropdownMenuItem onClick={() => api.openBackgroundsFolder().catch(() => {})}>
              <FolderOpen className="mr-2 h-4 w-4" />
              Dossier des fonds
            </DropdownMenuItem>
            {bg?.dir && (
              <div className="break-all px-2 py-1.5 text-[10px] text-muted-foreground">
                {bg.dir}
              </div>
            )}
          </DropdownMenuContent>
        </DropdownMenu>
      </div>

      {/* The invisible 10×6 grid. */}
      <div
        ref={gridRef}
        className="absolute inset-0 z-10 grid gap-3 p-4"
        style={{
          gridTemplateColumns: `repeat(${GRID_COLS}, minmax(0, 1fr))`,
          gridTemplateRows: `repeat(${GRID_ROWS}, minmax(0, 1fr))`,
        }}
      >
        {CARD_DEFS.filter((c) => layout[c.id]?.visible).map((c) => (
          <GridCard key={c.id} id={c.id} title={c.title} gridRef={gridRef}>
            {c.render({ trades })}
          </GridCard>
        ))}
      </div>
    </div>
  );
}
