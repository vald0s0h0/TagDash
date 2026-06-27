import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { CircleX, MoreVertical } from "lucide-react";
import { api } from "@/lib/tauri";
import { cn } from "@/lib/utils";
import { nyTime } from "@/lib/nyTime";
import type { Position } from "@/types";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";

function PosRow({ pos, onTickerClick }: { pos: Position; onTickerClick?: (symbol: string) => void }) {
  const qc      = useQueryClient();
  const [busy, setBusy] = useState(false);

  const isLong = pos.side === "long";
  const pnl    = pos.unrealized_pnl ?? 0;
  const r      = pos.r_multiple;

  async function handleClose() {
    setBusy(true);
    try {
      await api.closeInternalPosition(pos.symbol, pos.zone_id);
      qc.invalidateQueries({ queryKey: ["internal_positions"] });
    } catch (e) {
      console.error("close failed:", e);
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="flex items-center gap-1.5 border-b border-border/40 px-3 py-1.5 text-xs last:border-none hover:bg-accent/30">
      {/* Symbol + side badge */}
      <span className={cn(
        "w-4 shrink-0 rounded px-0.5 text-center text-[9px] font-bold",
        isLong ? "bg-emerald-900/60 text-emerald-400" : "bg-red-900/60 text-red-400"
      )}>
        {isLong ? "L" : "S"}
      </span>
      <button
        onClick={(e) => { e.stopPropagation(); onTickerClick?.(pos.symbol); }}
        className="w-10 shrink-0 font-semibold text-left hover:text-blue-400 hover:underline"
        title={`Ouvrir ${pos.symbol} dans le scanner`}
      >
        {pos.symbol}
      </button>

      {/* Qty */}
      <span className={cn(
        "w-8 shrink-0 tabular-nums",
        isLong ? "text-emerald-400" : "text-red-400"
      )}>
        {isLong ? "+" : ""}{pos.quantity}
      </span>

      {/* PnL */}
      <span className={cn(
        "flex-1 tabular-nums text-right",
        pnl >= 0 ? "text-emerald-400" : "text-red-400"
      )}>
        {pnl >= 0 ? "+" : ""}${pnl.toFixed(2)}
      </span>

      {/* R multiple */}
      {r != null && (
        <span className={cn(
          "w-8 shrink-0 tabular-nums text-right text-[10px]",
          r >= 0 ? "text-emerald-400/70" : "text-red-400/70"
        )}>
          {r >= 0 ? "+" : ""}{r.toFixed(1)}R
        </span>
      )}

      {/* Details dropdown */}
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <button className="shrink-0 rounded p-0.5 text-muted-foreground hover:bg-accent hover:text-foreground">
            <MoreVertical className="h-3 w-3" />
          </button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end" className="min-w-[13rem] p-2 text-xs">
          <div className="space-y-1 text-muted-foreground">
            <div className="flex justify-between">
              <span>Stratégie</span>
              <span className="text-foreground font-mono text-[10px]">{pos.strategy_id}</span>
            </div>
            <div className="flex justify-between">
              <span>Sens</span>
              <span className={isLong ? "text-emerald-400" : "text-red-400"}>
                {isLong ? "Long" : "Short"}
              </span>
            </div>
            <div className="flex justify-between">
              <span>Entrée</span>
              <span className="text-foreground tabular-nums">${pos.avg_entry_price.toFixed(2)}</span>
            </div>
            {pos.stop_loss != null && (
              <div className="flex justify-between">
                <span>SL</span>
                <span className="text-red-400 tabular-nums">${pos.stop_loss.toFixed(2)}</span>
              </div>
            )}
            {pos.take_profit != null && (
              <div className="flex justify-between">
                <span>TP</span>
                <span className="text-emerald-400 tabular-nums">${pos.take_profit.toFixed(2)}</span>
              </div>
            )}
            <div className="flex justify-between">
              <span>Depuis</span>
              <span className="text-foreground tabular-nums text-[10px]">
                {nyTime(pos.opened_at, true)}
              </span>
            </div>
          </div>
        </DropdownMenuContent>
      </DropdownMenu>

      {/* Close button */}
      <button
        disabled={busy}
        onClick={handleClose}
        title="Clôturer"
        className="shrink-0 rounded p-0.5 text-muted-foreground/60 hover:bg-red-900/30 hover:text-red-400 disabled:opacity-40"
      >
        <CircleX className="h-3.5 w-3.5" />
      </button>
    </div>
  );
}

export function PositionsPanel({ onTickerClick }: { onTickerClick?: (symbol: string) => void }) {
  const { data: positions = [] } = useQuery({
    queryKey: ["internal_positions"],
    queryFn:  () => api.getInternalPositions(),
    refetchInterval: 1000,
  });

  if (positions.length === 0) {
    return (
      <p className="px-3 py-2 text-xs text-muted-foreground/60">
        Aucune position ouverte.
      </p>
    );
  }

  return (
    <div className="flex flex-col overflow-y-auto">
      {positions.map((pos) => (
        <PosRow key={pos.trade_id} pos={pos} onTickerClick={onTickerClick} />
      ))}
    </div>
  );
}
