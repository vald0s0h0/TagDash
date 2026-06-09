import { useQuery } from "@tanstack/react-query";
import { Activity, AlertTriangle, CheckCircle2, Radio } from "lucide-react";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { api } from "@/lib/tauri";
import type { FeedDiagnostics } from "@/types";
import { cn } from "@/lib/utils";

interface Props {
  open: boolean;
  onClose: () => void;
}

const STATE_STYLE: Record<string, string> = {
  streaming:      "bg-emerald-900/50 text-emerald-300",
  subscribed:     "bg-blue-900/50 text-blue-300",
  authenticated:  "bg-blue-900/50 text-blue-300",
  authenticating: "bg-amber-900/50 text-amber-300",
  connecting:     "bg-amber-900/50 text-amber-300",
  reconnecting:   "bg-amber-900/50 text-amber-300",
  error:          "bg-red-900/60 text-red-300",
  stopped:        "bg-zinc-700 text-zinc-300",
  idle:           "bg-zinc-700 text-zinc-300",
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

function Row({ label, value }: { label: string; value: React.ReactNode }) {
  return (
    <div className="flex items-center justify-between border-b border-border/40 py-1.5 text-sm">
      <span className="text-muted-foreground">{label}</span>
      <span className="tabular-nums">{value}</span>
    </div>
  );
}

/** One-line plain-language read of the current feed health. */
function interpretation(d: FeedDiagnostics): { icon: typeof Activity; cls: string; text: string } {
  if (d.state === "error") {
    return {
      icon: AlertTriangle,
      cls:  "text-red-300",
      text: "Le flux est en erreur — voir le détail ci-dessous.",
    };
  }
  if (d.state === "streaming" && d.trades_received + d.quotes_received + d.bars_received > 0) {
    return {
      icon: CheckCircle2,
      cls:  "text-emerald-300",
      text: "Streaming actif — données reçues en temps réel.",
    };
  }
  if (d.state === "subscribed") {
    return {
      icon: Activity,
      cls:  "text-amber-300",
      text: "Abonné, en attente de données (hors séance, ou aucun trade pour l'instant).",
    };
  }
  if (["connecting", "authenticating", "reconnecting"].includes(d.state)) {
    return { icon: Activity, cls: "text-amber-300", text: "Connexion au flux en cours…" };
  }
  return { icon: Radio, cls: "text-muted-foreground", text: "Flux inactif." };
}

export function FeedDiagnosticsModal({ open, onClose }: Props) {
  const { data: d } = useQuery({
    queryKey: ["feed_diagnostics"],
    queryFn:  api.getFeedDiagnostics,
    enabled:  open,
    refetchInterval: 700,
  });

  const info = d ? interpretation(d) : null;
  const Icon = info?.icon ?? Radio;

  return (
    <Dialog open={open} onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="max-w-lg">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <Radio className="h-4 w-4" /> Diagnostic flux live (Alpaca)
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

            {/* Last error (prominent) */}
            {d.last_error_msg && (
              <div className="rounded border border-red-800/60 bg-red-950/40 p-2 text-xs text-red-300">
                <div className="font-semibold">
                  Dernière erreur{d.last_error_code ? ` — code ${d.last_error_code}` : ""}
                </div>
                <div className="mt-0.5 font-mono">{d.last_error_msg}</div>
                {d.last_error_code === 400 && (
                  <div className="mt-1 text-red-300/70">
                    « invalid syntax » = le message d'abonnement a été rejeté. Causes
                    fréquentes : trop de symboles pour le plan/flux, flux non autorisé
                    par l'abonnement, ou tickers au format invalide.
                  </div>
                )}
              </div>
            )}

            {/* Metrics */}
            <div className="rounded border border-border/40 px-3 py-1">
              <Row label="Flux" value={d.feed || "—"} />
              <Row
                label="Mode large"
                value={
                  d.broad_mode === "trades" ? "trades (premarket)"
                  : d.broad_mode === "bars" ? "bougies 1-min (open)"
                  : d.broad_mode || "—"
                }
              />
              <Row
                label="Surveillance large"
                value={
                  d.subscribed_symbols > 0
                    ? d.subscribed_symbols.toLocaleString()
                    : "tout le marché (✱)"
                }
              />
              <Row label="Dernier abonnement envoyé" value={d.last_subscribe || "—"} />
              <Row
                label="Confirmé par Alpaca"
                value={<span className="font-mono text-[11px]">{d.subscription_ack || "—"}</span>}
              />
              <Row
                label="Symboles focus (tick)"
                value={
                  <span className={d.focus_symbols > 0 ? "text-emerald-300" : undefined}>
                    {d.focus_symbols.toLocaleString()}
                  </span>
                }
              />
              <Row
                label="Tickers filtrés (invalides)"
                value={
                  <span className={d.invalid_symbols_dropped > 0 ? "text-amber-300" : undefined}>
                    {d.invalid_symbols_dropped.toLocaleString()}
                  </span>
                }
              />
              <Row label="Trades reçus" value={d.trades_received.toLocaleString()} />
              <Row label="Quotes reçus" value={d.quotes_received.toLocaleString()} />
              <Row label="Bougies 1-min reçues" value={d.bars_received.toLocaleString()} />
              <Row label="Dernier message" value={relTime(d.last_message_at)} />
              <Row label="Reconnexions" value={d.reconnects} />
              <Row label="Connecté depuis" value={relTime(d.connected_at)} />
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
