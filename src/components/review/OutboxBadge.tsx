// Sync indicator of the « Revue » tab. What the review writes onto TradeTally
// trades (edited notes, reviews) is saved locally at once and queued; the backend
// sends it in the background and retries until TradeTally accepts it. This shows
// what is still waiting, lets the trader retry now, or give up on a trade that
// TradeTally keeps refusing (deleted upstream…). Hidden when all is sent.

import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { AlertTriangle, CloudOff, LoaderCircle, RefreshCw, UploadCloud } from "lucide-react";
import { cn } from "@/lib/utils";
import { api } from "@/lib/tauri";
import type { ReviewOutboxStatus } from "@/types";
import { DropdownMenu, DropdownMenuContent, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";

const KEY = ["review-outbox"];

export function useReviewOutbox() {
  return useQuery({ queryKey: KEY, queryFn: api.reviewOutboxStatus, refetchInterval: 3000 });
}

/** Re-read the outbox now (right after something was queued). */
export function useRefreshOutbox() {
  const qc = useQueryClient();
  return () => qc.invalidateQueries({ queryKey: KEY });
}

/** An upstream refusal, as opposed to the network not reaching TradeTally. */
const isRejection = (e: string | null) => !!e && e.startsWith("HTTP ");

export function OutboxBadge() {
  const { data } = useReviewOutbox();
  const qc = useQueryClient();
  const [retrying, setRetrying] = useState(false);
  if (!data || (data.pending === 0 && !data.offline)) return null;

  const rejected = data.targets.filter((t) => isRejection(t.last_error));
  const busy = retrying || data.flushing;
  const label = data.pending === 0 ? "Hors ligne" : `${data.pending} en attente`;
  const Icon = busy ? LoaderCircle : rejected.length ? AlertTriangle : data.offline ? CloudOff : UploadCloud;

  const retry = async () => {
    setRetrying(true);
    try {
      qc.setQueryData<ReviewOutboxStatus>(KEY, await api.reviewOutboxFlush());
    } finally {
      setRetrying(false);
    }
  };
  const discard = async (ttId: string, what: string) => {
    if (!window.confirm(`Abandonner les modifications non envoyées de ${what} ? Elles restent dans TagDash mais n'iront pas dans TradeTally.`)) return;
    await api.reviewOutboxDiscard(ttId);
    qc.invalidateQueries({ queryKey: KEY });
  };

  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <button
          type="button"
          title={
            data.pending === 0
              ? "TradeTally injoignable — la revue continue en local"
              : "Modifications enregistrées localement, en attente d'envoi à TradeTally"
          }
          className={cn(
            "flex items-center gap-1 rounded px-1.5 py-0.5 text-[10px] tabular-nums",
            rejected.length
              ? "bg-red-600/15 text-red-400"
              : data.offline
                ? "bg-amber-500/15 text-amber-400"
                : "bg-sky-600/15 text-sky-400",
          )}
        >
          <Icon className={cn("h-3 w-3", busy && "animate-spin")} />
          {label}
        </button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="start" className="w-72 p-2 text-xs">
        <p className="mb-2 text-muted-foreground">
          {data.offline
            ? "TradeTally injoignable. Tout est enregistré dans TagDash et sera envoyé au retour de la connexion."
            : data.pending
              ? "Envoi à TradeTally en cours ou en attente de nouvel essai."
              : "Tout est synchronisé."}
        </p>
        {data.targets.length > 0 && (
          <div className="mb-2 max-h-48 space-y-1.5 overflow-y-auto">
            {data.targets.map((t) => {
              const what = [t.symbol ?? t.tt_id, t.day].filter(Boolean).join(" · ");
              return (
                <div key={t.tt_id} className="rounded border border-border px-2 py-1">
                  <div className="flex items-center gap-2">
                    <span className="font-medium">{what}</span>
                    <span className="text-muted-foreground">
                      {t.ops} modif{t.ops > 1 ? "s" : ""}
                    </span>
                    {isRejection(t.last_error) && (
                      <button
                        type="button"
                        onClick={() => discard(t.tt_id, what)}
                        className="ml-auto text-[10px] text-red-400 hover:text-red-300"
                      >
                        Abandonner
                      </button>
                    )}
                  </div>
                  {t.last_error && (
                    <p className="mt-0.5 truncate text-[10px] text-muted-foreground" title={t.last_error}>
                      {t.last_error}
                    </p>
                  )}
                </div>
              );
            })}
          </div>
        )}
        <button
          type="button"
          onClick={retry}
          disabled={busy}
          className="flex w-full items-center justify-center gap-1.5 rounded-md bg-accent px-2 py-1 text-xs hover:bg-accent/70 disabled:opacity-60"
        >
          <RefreshCw className={cn("h-3 w-3", busy && "animate-spin")} />
          Réessayer maintenant
        </button>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
