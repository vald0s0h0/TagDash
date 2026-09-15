import { notifyError } from "@/stores/notificationStore";
import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { CircleX, MoreVertical, X } from "lucide-react";
import { api } from "@/lib/tauri";
import { cn } from "@/lib/utils";
import { nyTime } from "@/lib/nyTime";
import type { InternalOrder, Position } from "@/types";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";

/** Pending entry orders not already folded into an open position's fill-progress
 *  count — i.e. no fill has landed yet for that trade. Reduce-only orders (the
 *  SL/TP bracket legs materialised once a position exists) are always excluded:
 *  their presence is shown by `LevelDots` on the position row instead. Exported
 *  so Sidebar's header count and this panel's render can never drift apart. */
export function standalonePendingOrders(
  orders: InternalOrder[],
  positions: Position[],
): InternalOrder[] {
  return orders.filter((o) => {
    if (o.reduce_only) return false;
    const matches = o.trade_id != null
      ? (p: Position) => p.trade_id === o.trade_id
      : (p: Position) => p.symbol === o.symbol;
    return !positions.some(matches);
  });
}

/** SL/TP presence, as two small dots stacked like ':' — green (top) for TP,
 *  red (bottom) for SL, dimmed when that level isn't set. Lets a row communicate
 *  "has a stop / has a target" without opening the details dropdown. */
function LevelDots({ hasTp, hasSl }: { hasTp: boolean; hasSl: boolean }) {
  return (
    <span className="flex shrink-0 flex-col items-center gap-0.5" title={
      hasTp && hasSl ? "SL & TP configurés"
        : hasTp ? "TP configuré (pas de SL)"
        : hasSl ? "SL configuré (pas de TP)"
        : "Aucun SL/TP"
    }>
      <span className={cn("h-1 w-1 rounded-full", hasTp ? "bg-emerald-500" : "bg-muted-foreground/20")} />
      <span className={cn("h-1 w-1 rounded-full", hasSl ? "bg-red-500" : "bg-muted-foreground/20")} />
    </span>
  );
}

function PosRow({ pos, onTickerClick }: { pos: Position; onTickerClick?: (symbol: string) => void }) {
  const qc      = useQueryClient();
  const [busy, setBusy] = useState(false);

  const isLong = pos.side === "long";
  const pnl    = pos.unrealized_pnl ?? 0;
  const r      = pos.r_multiple;
  const partial = pos.pending_entries > 0;

  async function handleClose() {
    setBusy(true);
    try {
      await api.closeInternalPosition(pos.symbol, pos.zone_id);
      qc.invalidateQueries({ queryKey: ["internal_positions"] });
    } catch (e) {
      notifyError(e, "Clôture");
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="flex items-center gap-1.5 border-b border-border/40 px-3 py-1.5 text-xs last:border-none hover:bg-accent/30">
      {/* Symbol + side badge — solid fill marks a live (filled) position */}
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

      {/* Partial-fill progress, only while a scaled-in leg is still resting */}
      {partial && (
        <span
          className="w-7 shrink-0 tabular-nums text-[10px] text-amber-400/80"
          title="Entrées remplies / total (entrée scalée en cours)"
        >
          {pos.filled_entries}/{pos.filled_entries + pos.pending_entries}
        </span>
      )}

      {/* PnL */}
      <span className={cn(
        "flex-1 tabular-nums text-right",
        pnl >= 0 ? "text-emerald-400" : "text-red-400"
      )}>
        {pnl >= 0 ? "+" : ""}${pnl.toFixed(2)}
      </span>

      {/* R multiple — risk is the SL frozen at entry, never the live/moved SL */}
      {r != null && (
        <span className={cn(
          "w-8 shrink-0 tabular-nums text-right text-[10px]",
          r >= 0 ? "text-emerald-400/70" : "text-red-400/70"
        )}>
          {r >= 0 ? "+" : ""}{r.toFixed(1)}R
        </span>
      )}

      <LevelDots hasTp={pos.take_profit != null} hasSl={pos.stop_loss != null} />

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
            {partial && (
              <div className="flex justify-between">
                <span>Rempli</span>
                <span className="text-foreground tabular-nums">
                  {pos.filled_entries}/{pos.filled_entries + pos.pending_entries}
                </span>
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

function PendingOrderRow({ order, onTickerClick }: { order: InternalOrder; onTickerClick?: (symbol: string) => void }) {
  const qc      = useQueryClient();
  const [busy, setBusy] = useState(false);

  const isLong = order.side === "long";

  async function handleCancel() {
    setBusy(true);
    try {
      await api.cancelInternalOrder(order.order_id);
      qc.invalidateQueries({ queryKey: ["internal_orders"] });
    } catch (e) {
      notifyError(e, "Annulation");
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="flex items-center gap-1.5 border-b border-border/40 px-3 py-1.5 text-xs last:border-none opacity-80 hover:bg-accent/30 hover:opacity-100">
      {/* Side badge — outline (not filled) marks a resting order, not a position */}
      <span className={cn(
        "w-4 shrink-0 rounded border px-0.5 text-center text-[9px] font-bold bg-transparent",
        isLong ? "border-emerald-700 text-emerald-400" : "border-red-700 text-red-400"
      )}>
        {isLong ? "L" : "S"}
      </span>

      {/* Symbol */}
      <button
        onClick={(e) => { e.stopPropagation(); onTickerClick?.(order.symbol); }}
        className="w-10 shrink-0 font-semibold text-left hover:text-blue-400 hover:underline"
        title={`Ouvrir ${order.symbol} dans le scanner`}
      >
        {order.symbol}
      </button>

      {/* Qty */}
      <span className="w-7 shrink-0 tabular-nums text-muted-foreground">
        {order.quantity}
      </span>

      {/* Limit price */}
      <span className="flex-1 tabular-nums text-muted-foreground" title="Ordre en attente">
        {order.limit_price != null ? `$${order.limit_price.toFixed(2)}` : "Mkt"}
      </span>

      <LevelDots hasTp={order.take_profit != null} hasSl={order.stop_loss != null} />

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
              <span>Sens</span>
              <span className={isLong ? "text-emerald-400" : "text-red-400"}>
                {isLong ? "Long" : "Short"}
              </span>
            </div>
            <div className="flex justify-between">
              <span>Type</span>
              <span className="text-foreground uppercase">{order.order_type}</span>
            </div>
            {order.limit_price != null && (
              <div className="flex justify-between">
                <span>Limit</span>
                <span className="text-foreground tabular-nums">${order.limit_price.toFixed(2)}</span>
              </div>
            )}
            {order.stop_loss != null && (
              <div className="flex justify-between">
                <span>SL</span>
                <span className="text-red-400 tabular-nums">${order.stop_loss.toFixed(2)}</span>
              </div>
            )}
            {order.take_profit != null && (
              <div className="flex justify-between">
                <span>TP</span>
                <span className="text-emerald-400 tabular-nums">${order.take_profit.toFixed(2)}</span>
              </div>
            )}
            <div className="flex justify-between">
              <span>Créé à</span>
              <span className="text-foreground tabular-nums text-[10px]">
                {nyTime(order.created_at, true)}
              </span>
            </div>
          </div>
        </DropdownMenuContent>
      </DropdownMenu>

      {/* Cancel button */}
      <button
        disabled={busy}
        onClick={handleCancel}
        title="Annuler"
        className="shrink-0 rounded p-0.5 text-muted-foreground/60 hover:bg-red-900/30 hover:text-red-400 disabled:opacity-40"
      >
        <X className="h-3.5 w-3.5" />
      </button>
    </div>
  );
}

export function OrdersPositionsPanel({ onTickerClick }: { onTickerClick?: (symbol: string) => void }) {
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

  const pendingOrders = standalonePendingOrders(orders, positions);

  if (positions.length === 0 && pendingOrders.length === 0) {
    return (
      <p className="px-3 py-2 text-xs text-muted-foreground/60">
        Aucun ordre ni position.
      </p>
    );
  }

  return (
    <div className="flex flex-col overflow-y-auto">
      {positions.map((pos) => (
        <PosRow key={pos.trade_id} pos={pos} onTickerClick={onTickerClick} />
      ))}
      {pendingOrders.map((order) => (
        <PendingOrderRow key={order.order_id} order={order} onTickerClick={onTickerClick} />
      ))}
    </div>
  );
}
