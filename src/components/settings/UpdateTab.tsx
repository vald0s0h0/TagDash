// Settings → Comptes & Système → Mise à jour. Ported from the former UpdateModal —
// same store/content, just dropped the Dialog wrapper.

import { useEffect, useState } from "react";
import {
  CheckCircle2,
  Copy,
  DownloadCloud,
  Loader2,
  RefreshCw,
  TriangleAlert,
} from "lucide-react";
import { Button } from "@/components/ui/button";
import { ScrollArea } from "@/components/ui/scroll-area";
import { Separator } from "@/components/ui/separator";
import { cn } from "@/lib/utils";
import { useUpdaterStore } from "@/stores/updaterStore";

// The updater endpoint the app polls — surfaced read-only for debugging.
const UPDATE_ENDPOINT =
  "https://github.com/vald0s0h0/TagDash/releases/latest/download/latest.json";

export function UpdateTab() {
  const status = useUpdaterStore((s) => s.status);
  const version = useUpdaterStore((s) => s.version);
  const currentVersion = useUpdaterStore((s) => s.currentVersion);
  const progress = useUpdaterStore((s) => s.progress);
  const error = useUpdaterStore((s) => s.error);
  const logs = useUpdaterStore((s) => s.logs);
  const busy = useUpdaterStore((s) => s.busy);
  const checkNow = useUpdaterStore((s) => s.checkNow);
  const installNow = useUpdaterStore((s) => s.installNow);
  const loadCurrentVersion = useUpdaterStore((s) => s.loadCurrentVersion);

  const [copied, setCopied] = useState(false);

  // On mount: make sure the installed version is known, then run a fresh check so
  // the tab reflects reality without the user clicking anything. Guarded by `busy`
  // inside the store so it won't fight the launch auto-run.
  useEffect(() => {
    void loadCurrentVersion();
    void checkNow();
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  const updateAvailable =
    status === "available" || status === "downloading" || status === "installing";
  const inProgress = status === "downloading" || status === "installing";

  const statusLabel =
    status === "checking"      ? "Recherche d'une mise à jour…"
    : status === "available"   ? `Mise à jour ${version ?? ""} disponible`
    : status === "downloading" ? `Téléchargement… ${Math.round(progress * 100)} %`
    : status === "installing"  ? "Installation… (redémarrage imminent)"
    : status === "uptodate"    ? "Application à jour"
    : status === "disabled"    ? "Mise à jour ignorée (build de développement)"
    : status === "error"       ? "Vérification indisponible"
    : "Prêt à vérifier";

  const StatusIcon =
    status === "uptodate" ? CheckCircle2
    : status === "error" ? TriangleAlert
    : updateAvailable && !inProgress ? DownloadCloud
    : status === "checking" || inProgress ? Loader2
    : RefreshCw;

  const statusColor =
    status === "uptodate" ? "text-emerald-400"
    : status === "error" ? "text-red-400"
    : updateAvailable ? "text-amber-400"
    : "text-muted-foreground";

  const handleCopyLogs = async () => {
    try {
      await navigator.clipboard.writeText(logs.join("\n"));
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {
      /* clipboard may be unavailable — ignore */
    }
  };

  return (
    <div className="space-y-3">
      {/* Versions */}
      <div className="flex items-center justify-between rounded-md border border-border bg-card px-3 py-2 text-sm">
        <span className="text-muted-foreground">Version installée</span>
        <span className="font-semibold tabular-nums">
          {currentVersion ?? "—"}
        </span>
      </div>
      {updateAvailable && version && (
        <div className="flex items-center justify-between rounded-md border border-amber-700/50 bg-amber-900/10 px-3 py-2 text-sm">
          <span className="text-amber-400">Version disponible</span>
          <span className="font-semibold tabular-nums text-amber-300">
            {version}
          </span>
        </div>
      )}

      {/* Status line */}
      <div className={cn("flex items-center gap-2 text-sm", statusColor)}>
        <StatusIcon
          className={cn(
            "h-4 w-4",
            (status === "checking" || inProgress) && "animate-spin"
          )}
        />
        <span>{statusLabel}</span>
      </div>

      {/* Progress bar while downloading */}
      {status === "downloading" && (
        <div className="h-1.5 w-full overflow-hidden rounded-full bg-muted">
          <div
            className="h-full bg-amber-400 transition-[width]"
            style={{ width: `${Math.round(progress * 100)}%` }}
          />
        </div>
      )}

      {/* Error detail */}
      {status === "error" && error && (
        <p className="rounded-md border border-red-700/40 bg-red-900/10 px-3 py-2 text-[11px] text-red-400">
          {error}
        </p>
      )}

      {/* Actions */}
      <div className="flex flex-wrap gap-2">
        <Button
          variant="outline"
          size="sm"
          onClick={() => void checkNow()}
          disabled={busy}
        >
          <RefreshCw className={cn("h-4 w-4", busy && "animate-spin")} />
          Vérifier maintenant
        </Button>
        <Button
          size="sm"
          onClick={() => void installNow()}
          disabled={busy || inProgress || status === "disabled"}
          title={
            updateAvailable
              ? "Télécharger, installer et redémarrer"
              : "Forcer une vérification puis installer si une version existe"
          }
        >
          <DownloadCloud className="h-4 w-4" />
          {updateAvailable ? "Installer et redémarrer" : "Forcer la mise à jour"}
        </Button>
      </div>

      <Separator />

      {/* Debug log */}
      <div className="flex items-center justify-between">
        <p className="text-[10px] uppercase tracking-wider text-muted-foreground">
          Logs de débogage
        </p>
        <button
          onClick={handleCopyLogs}
          disabled={logs.length === 0}
          className="flex items-center gap-1 rounded px-1.5 py-0.5 text-[10px] text-muted-foreground hover:bg-accent hover:text-foreground disabled:opacity-40"
          title="Copier les logs"
        >
          <Copy className="h-3 w-3" />
          {copied ? "Copié !" : "Copier"}
        </button>
      </div>
      <ScrollArea className="h-40 rounded-md border border-border bg-black/30 p-2">
        {logs.length === 0 ? (
          <p className="text-[11px] text-muted-foreground">Aucun log pour le moment.</p>
        ) : (
          <pre className="whitespace-pre-wrap break-words font-mono text-[10px] leading-relaxed text-muted-foreground">
            {logs.join("\n")}
          </pre>
        )}
      </ScrollArea>
      <p className="break-all font-mono text-[9px] text-muted-foreground/60">
        endpoint : {UPDATE_ENDPOINT}
      </p>
    </div>
  );
}
