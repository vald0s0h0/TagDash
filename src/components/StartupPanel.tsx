import {
  CheckCircle2,
  AlertTriangle,
  XCircle,
  Loader2,
  Circle,
  Play,
  ChevronDown,
  ChevronRight,
} from "lucide-react";
import { useState } from "react";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Separator } from "@/components/ui/separator";
import { ScrollArea } from "@/components/ui/scroll-area";
import { cn } from "@/lib/utils";
import {
  useStartupStatus,
  useRunStartupPipeline,
  useStreamableUniverse,
  useRefetchUniverseOnComplete,
} from "@/queries/useStartup";
import { useUpdaterStore } from "@/stores/updaterStore";
import type { StartupStep, StepStatus, StreamableSymbol } from "@/types";

// ─── Step icon ────────────────────────────────────────────────────────────────

function StepIcon({ status }: { status: StepStatus }) {
  switch (status) {
    case "running":
      return <Loader2 className="h-4 w-4 animate-spin text-blue-400" />;
    case "success":
      return <CheckCircle2 className="h-4 w-4 text-emerald-400" />;
    case "warning":
      return <AlertTriangle className="h-4 w-4 text-amber-400" />;
    case "failed":
      return <XCircle className="h-4 w-4 text-red-400" />;
    default:
      return <Circle className="h-4 w-4 text-muted-foreground/40" />;
  }
}

function StepRow({ step }: { step: StartupStep }) {
  return (
    <div className="flex items-start gap-3 py-1.5">
      <div className="mt-0.5 shrink-0">
        <StepIcon status={step.status} />
      </div>
      <div className="flex-1 min-w-0">
        <span
          className={cn(
            "text-sm",
            step.status === "pending" && "text-muted-foreground/50",
            step.status === "running" && "text-foreground font-medium",
            step.status === "success" && "text-foreground",
            step.status === "warning" && "text-amber-300",
            step.status === "failed" && "text-red-400"
          )}
        >
          {step.label}
        </span>
        {step.detail && (
          <p className="mt-0.5 text-[11px] text-muted-foreground">{step.detail}</p>
        )}
      </div>
    </div>
  );
}

// ─── Auto-update step (frontend-driven, always the first row) ──────────────────

/** The launch-time auto-update check, shown as the first step of the pipeline.
 *  Reflects the `updaterStore` flow (check → download → install → relaunch). */
function UpdaterStep() {
  const status = useUpdaterStore((s) => s.status);
  const version = useUpdaterStore((s) => s.version);
  const progress = useUpdaterStore((s) => s.progress);
  const error = useUpdaterStore((s) => s.error);

  const stepStatus: StepStatus =
    status === "checking" || status === "available" ||
    status === "downloading" || status === "installing"
      ? "running"
      : status === "uptodate"
        ? "success"
        : status === "error"
          ? "warning"
          : "pending";

  const label =
    status === "checking"      ? "Recherche d'une mise à jour…"
    : status === "available"   ? `Mise à jour ${version ?? ""} disponible`
    : status === "downloading" ? `Téléchargement de la mise à jour… ${Math.round(progress * 100)} %`
    : status === "installing"  ? "Installation de la mise à jour…"
    : status === "uptodate"    ? "Application à jour"
    : status === "error"       ? "Vérification des mises à jour indisponible"
    : status === "disabled"    ? "Mises à jour (ignorées en développement)"
    : "Vérifier les mises à jour";

  const detail =
    status === "available" || status === "downloading" || status === "installing"
      ? "Redémarrage automatique après installation"
      : status === "error"
        ? error
        : null;

  return <StepRow step={{ id: "auto_update", label, status: stepStatus, detail }} />;
}

// ─── Stats bar ────────────────────────────────────────────────────────────────

function StatBox({
  label,
  value,
  color,
}: {
  label: string;
  value: number;
  color: string;
}) {
  return (
    <div className="flex flex-col items-center gap-0.5 rounded-md border border-border bg-card px-4 py-3">
      <span className={cn("text-2xl font-bold tabular-nums", color)}>{value}</span>
      <span className="text-center text-[10px] uppercase tracking-wide text-muted-foreground">
        {label}
      </span>
    </div>
  );
}

// ─── Universe table ──────────────────────────────────────────────────────────

function UniverseTable({ symbols }: { symbols: StreamableSymbol[] }) {
  const fmt = (n: number | null, div = 1) =>
    n == null ? "—" : (n / div).toLocaleString("en-US", { maximumFractionDigits: 1 });

  return (
    <ScrollArea className="h-52 rounded-md border border-border">
      <table className="w-full text-xs">
        <thead className="sticky top-0 bg-card">
          <tr className="border-b border-border text-[10px] uppercase tracking-wider text-muted-foreground">
            <th className="px-3 py-2 text-left">Symbol</th>
            <th className="px-3 py-2 text-left">Exchange</th>
            <th className="px-3 py-2 text-left">Country</th>
            <th className="px-3 py-2 text-left">Industry</th>
            <th className="px-3 py-2 text-right">Float (M)</th>
            <th className="px-3 py-2 text-right">Avg Vol</th>
            <th className="px-3 py-2 text-center">Short</th>
          </tr>
        </thead>
        <tbody>
          {symbols.map((s) => (
            <tr
              key={s.symbol}
              className="border-b border-border/30 hover:bg-accent/20"
            >
              <td className="px-3 py-1.5 font-semibold">{s.symbol}</td>
              <td className="px-3 py-1.5 text-muted-foreground">
                {s.exchange ?? "—"}
              </td>
              <td className="px-3 py-1.5 text-muted-foreground">
                {s.country ?? "—"}
              </td>
              <td className="px-3 py-1.5 text-muted-foreground truncate max-w-[180px]" title={s.industry ?? undefined}>
                {s.industry ?? "—"}
              </td>
              <td className="px-3 py-1.5 text-right tabular-nums">
                {fmt(s.float_shares, 1_000_000)}
              </td>
              <td className="px-3 py-1.5 text-right tabular-nums">
                {fmt(s.avg_volume)}
              </td>
              <td className="px-3 py-1.5 text-center">
                {s.shortable ? (
                  <span className="text-emerald-400">✓</span>
                ) : (
                  <span className="text-muted-foreground/40">—</span>
                )}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </ScrollArea>
  );
}

// ─── Main component ───────────────────────────────────────────────────────────

export function StartupPanel() {
  const { data: startupData } = useStartupStatus();
  const { data: universe } = useStreamableUniverse();
  const run = useRunStartupPipeline();
  const [universeOpen, setUniverseOpen] = useState(false);

  // Refresh the universe table once the pipeline finishes writing its data.
  useRefetchUniverseOnComplete(startupData?.completed);

  const stats = startupData?.stats ?? {
    cache_symbols: 0,
    alpaca_active: 0,
    with_float: 0,
    final_universe: 0,
  };

  const isRunning = startupData != null && !startupData.completed
    && startupData.steps.some((s) => s.status === "running");

  return (
    <div className="flex flex-col gap-4 p-6 max-w-2xl mx-auto w-full">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div>
          <h2 className="text-lg font-semibold">Startup Pipeline</h2>
          <p className="text-xs text-muted-foreground mt-0.5">
            {isRunning
              ? "Préparation de l'univers de trading en cours…"
              : "Prepares the trading universe before connecting to Alpaca WebSocket"}
          </p>
        </div>
        <div className="flex items-center gap-2">
          {startupData?.mock_mode && (
            <Badge variant="outline" className="text-amber-400 border-amber-700 text-[10px]">
              MOCK MODE
            </Badge>
          )}
          {startupData?.completed && (
            <Button
              size="sm"
              variant="outline"
              onClick={() => run.mutate()}
              disabled={run.isPending}
            >
              <Play className="mr-1.5 h-3.5 w-3.5" />
              Relancer
            </Button>
          )}
        </div>
      </div>

      {/* Warnings */}
      {startupData?.warnings && startupData.warnings.length > 0 && (
        <div className="space-y-1.5">
          {startupData.warnings.map((w, i) => (
            <div
              key={i}
              className="flex items-start gap-2 rounded-md border border-amber-800/40 bg-amber-950/20 px-3 py-2 text-xs text-amber-300"
            >
              <AlertTriangle className="mt-0.5 h-3.5 w-3.5 shrink-0" />
              {w}
            </div>
          ))}
        </div>
      )}

      {/* Steps — the auto-update check is always rendered first, even before the
          backend pipeline status has loaded. */}
      <div className="rounded-lg border border-border bg-card px-4 py-2">
        <UpdaterStep />
        {startupData ? (
          startupData.steps.map((step) => (
            <div key={step.id}>
              <div className="ml-7 h-px bg-border/30" />
              <StepRow step={step} />
            </div>
          ))
        ) : (
          <>
            <div className="ml-7 h-px bg-border/30" />
            <div className="flex items-center gap-2 py-2 pl-7 text-sm text-muted-foreground">
              <Loader2 className="h-4 w-4 animate-spin" />
              Initialisation…
            </div>
          </>
        )}
      </div>

      {/* Stats */}
      {startupData && (
        <div className="grid grid-cols-4 gap-3">
          <StatBox label="Cache" value={stats.cache_symbols} color="text-muted-foreground" />
          <StatBox label="Alpaca active" value={stats.alpaca_active} color="text-blue-400" />
          <StatBox label="US Stocks" value={stats.final_universe} color="text-emerald-400" />
          <StatBox label="With float" value={stats.with_float} color="text-amber-400" />
        </div>
      )}

      {/* Universe table (collapsible) */}
      {startupData?.completed && universe && universe.length > 0 && (
        <div>
          <button
            className="flex items-center gap-1.5 text-xs text-muted-foreground hover:text-foreground mb-2"
            onClick={() => setUniverseOpen((o) => !o)}
          >
            {universeOpen ? (
              <ChevronDown className="h-3.5 w-3.5" />
            ) : (
              <ChevronRight className="h-3.5 w-3.5" />
            )}
            Streamable universe ({universe.length} symbols)
          </button>
          {universeOpen && <UniverseTable symbols={universe} />}
        </div>
      )}

      {/* Completion note */}
      {startupData?.completed && (
        <>
          <Separator />
          <p className="text-xs text-muted-foreground text-center">
            Pipeline complete — Alpaca WebSocket live feed started
          </p>
        </>
      )}
    </div>
  );
}
