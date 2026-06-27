import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { MoreVertical, X } from "lucide-react";
import { api } from "@/lib/tauri";
import { cn } from "@/lib/utils";
import { nyTime } from "@/lib/nyTime";
import type { InternalOrder } from "@/types";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";

// OCO indicator: two green dots joined by a line
function OcoBadge() {
  return (
    <span title="OCO — SL & TP configurés" className="flex items-center gap-0.5">
      <span className="h-1.5 w-1.5 rounded-full bg-emerald-500" />
      <span className="h-px w-2 bg-emerald-500/60" />
      <span className="h-1.5 w-1.5 rounded-full bg-emerald-500" />
    </span>
  );
}

function OrderRow({ order, onTickerClick }: { order: InternalOrder; onTickerClick?: (symbol: string) => void }) {
  const qc      = useQueryClient();
  const [busy, setBusy] = useState(false);

  const isLong = order.side === "long";

  async function handleCancel() {
    setBusy(true);
    try {
      await api.cancelInternalOrder(order.order_id);
      qc.invalidateQueries({ queryKey: ["internal_orders"] });
    } catch (e) {
      console.error("cancel failed:", e);
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="flex items-center gap-1.5 border-b border-border/40 px-3 py-1.5 text-xs last:border-none hover:bg-accent/30">
      {/* Side badge */}
      <span className={cn(
        "w-4 shrink-0 rounded px-0.5 text-center text-[9px] font-bold",
        isLong ? "bg-emerald-900/60 text-emerald-400" : "bg-red-900/60 text-red-400"
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
      <span className="flex-1 tabular-nums">
        {order.limit_price != null ? `$${order.limit_price.toFixed(2)}` : "Mkt"}
      </span>

      {/* OCO indicator */}
      {order.oco_group && <OcoBadge />}

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

export function OrdersPanel({ onTickerClick }: { onTickerClick?: (symbol: string) => void }) {
  const { data: orders = [] } = useQuery({
    queryKey: ["internal_orders"],
    queryFn:  () => api.getInternalOrders(),
    refetchInterval: 1000,
  });

  if (orders.length === 0) {
    return (
      <p className="px-3 py-2 text-xs text-muted-foreground/60">
        Aucun ordre en attente.
      </p>
    );
  }

  return (
    <div className="flex flex-col overflow-y-auto">
      {orders.map((order) => (
        <OrderRow key={order.order_id} order={order} onTickerClick={onTickerClick} />
      ))}
    </div>
  );
}
