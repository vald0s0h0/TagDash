import { useQuery } from "@tanstack/react-query";
import { AlertTriangle, CheckCircle2, Clock, Newspaper } from "lucide-react";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { api } from "@/lib/tauri";
import { nyTime } from "@/lib/nyTime";
import type { NewsDiagnostics } from "@/types";
import { cn } from "@/lib/utils";

interface Props {
  open: boolean;
  onClose: () => void;
}

const STATE_STYLE: Record<string, string> = {
  streaming:          "bg-emerald-900/50 text-emerald-300",
  subscribed:         "bg-blue-900/50 text-blue-300",
  authenticated:      "bg-blue-900/50 text-blue-300",
  connecting:         "bg-amber-900/50 text-amber-300",
  waiting_premarket:  "bg-zinc-700 text-zinc-300",
  error:              "bg-red-900/60 text-red-300",
  stopped:            "bg-zinc-700 text-zinc-300",
  idle:               "bg-zinc-700 text-zinc-300",
};

function relTime(iso: string | null): string {
  if (!iso) return "—";
  try {
    const diff = Date.now() - new Date(iso).getTime();
    if (diff < 1_000)     return "à l'instant";
    if (diff < 60_000)    return `il y a ${Math.round(diff / 1000)}s`;
    if (diff < 3_600_000) return `il y a ${Math.round(diff / 60_000)}min`;
    return `il y a ${Math.round(diff / 3_600_000)}h`;
  } catch {
    return "—";
  }
}

function clockTime(iso: string): string {
  try {
    return nyTime(iso, true); // New York time
  } catch {
    return iso;
  }
}

function Row({ label, value }: { label: string; value: React.ReactNode }) {
  return (
    <div className="flex items-center justify-between border-b border-border/40 py-1.5 text-sm">
      <span className="text-muted-foreground">{label}</span>
      <span className="tabular-nums">{value}</span>
    </div>
  );
}

/** One-line plain-language read of the news feed health. */
function interpretation(d: NewsDiagnostics): { icon: typeof Newspaper; cls: string; text: string } {
  if (d.state === "error") {
    return { icon: AlertTriangle, cls: "text-red-300", text: "Le flux news est en erreur — voir ci-dessous." };
  }
  if (d.state === "streaming" && d.news_received > 0) {
    return { icon: CheckCircle2, cls: "text-emerald-300", text: "News reçues en temps réel." };
  }
  if (d.state === "subscribed") {
    return { icon: Newspaper, cls: "text-amber-300", text: "Abonné — en attente de la prochaine news." };
  }
  if (d.state === "waiting_premarket") {
    return { icon: Clock, cls: "text-muted-foreground", text: "Hors premarket (4h–9h30 ET) — flux en veille." };
  }
  if (["connecting", "authenticated"].includes(d.state)) {
    return { icon: Newspaper, cls: "text-amber-300", text: "Connexion au flux news en cours…" };
  }
  return { icon: Newspaper, cls: "text-muted-foreground", text: "Flux news inactif." };
}

export function NewsDebugModal({ open, onClose }: Props) {
  const { data: d } = useQuery({
    queryKey: ["news_diagnostics"],
    queryFn:  api.getNewsDiagnostics,
    enabled:  open,
    refetchInterval: 1000,
  });

  const info = d ? interpretation(d) : null;
  const Icon = info?.icon ?? Newspaper;

  return (
    <Dialog open={open} onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="max-w-lg">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <Newspaper className="h-4 w-4" /> Debug news premarket (Alpaca)
          </DialogTitle>
        </DialogHeader>

        {!d ? (
          <p className="py-6 text-center text-sm text-muted-foreground">Chargement…</p>
        ) : (
          <div className="mt-2 space-y-3">
            {/* State + interpretation */}
            <div className="flex items-center gap-2">
              <span
                className={cn(
                  "rounded px-2 py-0.5 text-xs font-bold uppercase tracking-wide",
                  STATE_STYLE[d.state] ?? STATE_STYLE.idle,
                )}
              >
                {d.state}
              </span>
              {info && (
                <span className={cn("flex items-center gap-1.5 text-xs", info.cls)}>
                  <Icon className="h-3.5 w-3.5" />
                  {info.text}
                </span>
              )}
            </div>

            {/* Last error */}
            {d.last_error && (
              <div className="rounded border border-red-800/60 bg-red-950/40 p-2 text-xs text-red-300">
                <div className="font-semibold">Dernière erreur</div>
                <div className="mt-0.5 font-mono">{d.last_error}</div>
              </div>
            )}

            {/* Metrics */}
            <div className="rounded border border-border/40 px-3 py-1">
              <Row
                label="Premarket actif"
                value={
                  <span className={d.in_premarket ? "text-emerald-300" : "text-muted-foreground"}>
                    {d.in_premarket ? "oui" : "non"}
                  </span>
                }
              />
              <Row
                label="News reçues"
                value={
                  <span className={d.news_received > 0 ? "text-emerald-300" : undefined}>
                    {d.news_received.toLocaleString()}
                  </span>
                }
              />
              <Row label="Tickers avec news" value={d.symbols_with_news.toLocaleString()} />
              <Row label="Dernière news" value={relTime(d.last_news_at)} />
              <Row label="Connecté depuis" value={relTime(d.connected_at)} />
            </div>

            {/* Recent headlines feed */}
            <div>
              <div className="mb-1 text-xs font-semibold text-muted-foreground">
                Dernières news ({d.recent.length})
              </div>
              <div className="max-h-72 space-y-1.5 overflow-y-auto pr-1">
                {d.recent.length === 0 ? (
                  <p className="py-4 text-center text-xs text-muted-foreground">
                    Aucune news reçue pour l'instant.
                  </p>
                ) : (
                  d.recent.map((n, i) => (
                    <div
                      key={`${n.id}-${i}`}
                      className="rounded border border-border/40 bg-card/40 p-2 text-xs"
                    >
                      <div className="flex items-center gap-2">
                        <span className="font-mono tabular-nums text-muted-foreground">
                          {clockTime(n.created_at)}
                        </span>
                        <div className="flex flex-wrap gap-1">
                          {n.symbols.length === 0 ? (
                            <span className="text-muted-foreground/60">(aucun ticker)</span>
                          ) : (
                            n.symbols.slice(0, 6).map((s) => (
                              <span
                                key={s}
                                className="rounded bg-blue-900/40 px-1 font-mono font-semibold text-blue-300"
                              >
                                {s}
                              </span>
                            ))
                          )}
                        </div>
                        {n.source && (
                          <span className="ml-auto text-muted-foreground/60">{n.source}</span>
                        )}
                      </div>
                      <div className="mt-1 text-foreground/90">{n.headline}</div>
                    </div>
                  ))
                )}
              </div>
            </div>

            <p className="text-[10px] text-muted-foreground/60">
              Mis à jour {relTime(d.updated_at)} · rafraîchi automatiquement.
            </p>
          </div>
        )}
      </DialogContent>
    </Dialog>
  );
}
