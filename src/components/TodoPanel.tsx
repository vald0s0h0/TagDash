import { useState } from "react";
import { Camera, NotebookPen } from "lucide-react";
import { useQuery } from "@tanstack/react-query";
import { api } from "@/lib/tauri";
import { cn } from "@/lib/utils";
import { JournalModal } from "./JournalModal";
import type { TodoTrade } from "@/types";

function badgeColor(t: TodoTrade): string {
  if (t.pnl === 0) return "bg-zinc-700 text-zinc-300";
  if (t.open && t.pnl > 0) return "bg-emerald-600 text-white";
  if (t.open && t.pnl < 0) return "bg-red-600 text-white";
  if (!t.open && t.pnl > 0) return "bg-emerald-900/60 text-emerald-300";
  return "bg-red-900/60 text-red-300";
}

export function TodoPanel({ onTickerClick }: { onTickerClick?: (symbol: string) => void }) {
  const { data: todos = [] } = useQuery({
    queryKey: ["todo_trades"],
    queryFn:  () => api.getTodoTrades(),
    refetchInterval: 3000,
  });

  const [journalTarget, setJournalTarget] =
    useState<{ tradeId: string; symbol: string } | null>(null);

  if (todos.length === 0) {
    return (
      <p className="px-3 py-2 text-xs text-muted-foreground/60">
        Tout est à jour.
      </p>
    );
  }

  return (
    <>
      <div className="flex flex-wrap gap-1 px-2 py-1.5">
        {todos.map((t) => (
          <span
            key={t.trade_id}
            className={cn(
              "inline-flex items-center gap-1 rounded-full px-2 py-0.5 text-[11px] font-medium leading-tight",
              badgeColor(t),
            )}
          >
            <button
              onClick={() => onTickerClick?.(t.symbol)}
              className="hover:underline"
              title={`${t.symbol} · ${t.open ? "ouvert" : "fermé"} · ${t.pnl >= 0 ? "+" : ""}$${t.pnl.toFixed(0)}`}
            >
              {t.symbol}
            </button>
            {!t.has_screenshot && (
              <Camera className="h-2.5 w-2.5 opacity-70" />
            )}
            {!t.has_journal && (
              <button
                onClick={(e) => {
                  e.stopPropagation();
                  setJournalTarget({ tradeId: t.trade_id, symbol: t.symbol });
                }}
                title="Ouvrir le journal"
              >
                <NotebookPen className="h-2.5 w-2.5 opacity-70 hover:opacity-100" />
              </button>
            )}
          </span>
        ))}
      </div>

      {journalTarget && (
        <JournalModal
          open={true}
          onClose={() => setJournalTarget(null)}
          tradeId={journalTarget.tradeId}
          symbol={journalTarget.symbol}
        />
      )}
    </>
  );
}
