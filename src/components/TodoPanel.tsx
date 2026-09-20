import { useState } from "react";
import { Camera, Eraser, NotebookPen } from "lucide-react";
import { useQuery } from "@tanstack/react-query";
import { api } from "@/lib/tauri";
import { cn } from "@/lib/utils";
import { useTodoClearStore } from "@/stores/todoClearStore";
import { JournalModal } from "./JournalModal";
import type { TodoTrade } from "@/types";

/** The rows actually rendered: everything the user hasn't cleared. The header's
 *  count and the section's visibility read this too, so badge and list can never
 *  disagree (same contract as the screener's `visibleMatches`). */
export function visibleTodos(todos: TodoTrade[], cleared: string[]): TodoTrade[] {
  return cleared.length === 0
    ? todos
    : todos.filter((t) => !cleared.includes(t.trade_id));
}

/** "Vider" — empties the À faire list without touching a single trade: it just
 *  records the pending trade ids as cleared. Sits to the LEFT of the count badge. */
export function TodoClearButton({ todos }: { todos: TodoTrade[] }) {
  const clearAll = useTodoClearStore((s) => s.clearAll);
  return (
    <button
      type="button"
      onClick={() => clearAll(todos.map((t) => t.trade_id))}
      title="Vider la liste À faire (les trades ne sont pas supprimés)"
      className={cn(
        "flex items-center gap-1 rounded px-1.5 py-0.5 text-[10px] font-semibold uppercase tracking-wide transition-colors",
        "bg-accent text-muted-foreground hover:bg-accent/70 hover:text-foreground",
      )}
    >
      <Eraser className="h-3 w-3" />
      <span>Vider</span>
    </button>
  );
}

function badgeColor(t: TodoTrade): string {
  if (t.pnl === 0) return "bg-zinc-700 text-zinc-300";
  if (t.open && t.pnl > 0) return "bg-emerald-600 text-white";
  if (t.open && t.pnl < 0) return "bg-red-600 text-white";
  if (!t.open && t.pnl > 0) return "bg-emerald-900/60 text-emerald-300";
  return "bg-red-900/60 text-red-300";
}

export function TodoPanel({ onTickerClick }: { onTickerClick?: (symbol: string) => void }) {
  const { data: allTodos = [] } = useQuery({
    queryKey: ["todo_trades"],
    queryFn:  () => api.getTodoTrades(),
    refetchInterval: 3000,
  });
  const cleared = useTodoClearStore((s) => s.cleared);
  const todos = visibleTodos(allTodos, cleared);

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
