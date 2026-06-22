// Gestion Flat Files — download, browse and share offline market-data days.
//
// Downloads 1-minute bars (premarket + regular + after-hours) of the liquid US
// universe per trading day into `<app_dir>/flat_files/flat-YYYY-MM-DD.db`, so the
// platform can run entirely offline through Market Replay (useful if the Alpaca
// API becomes unavailable, and to share a day with another TagDash user — just
// copy the `.db` file into their folder). A month calendar colour-codes which days
// are available on disk. The data-source toggle (API ↔ Flat files) is duplicated
// here and in Settings → API Keys; the choice is persisted in tagdash.toml.

import { useEffect, useMemo, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import {
  ChevronLeft,
  ChevronRight,
  Database,
  Download,
  FolderOpen,
  Loader2,
  Radio,
  Square,
} from "lucide-react";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { api } from "@/lib/tauri";
import { cn } from "@/lib/utils";
import { useLocalConfig, useUpdateLocalConfig } from "@/queries/useLocalConfig";
import type { FlatFileDay, FlatFilesStatus } from "@/types";

interface Props {
  open: boolean;
  onClose: () => void;
}

const WEEKDAY_LABELS = ["Lun", "Mar", "Mer", "Jeu", "Ven", "Sam", "Dim"];
const MONTH_LABELS = [
  "Janvier", "Février", "Mars", "Avril", "Mai", "Juin",
  "Juillet", "Août", "Septembre", "Octobre", "Novembre", "Décembre",
];

/** Local YYYY-MM-DD (no timezone shift). */
function ymd(d: Date): string {
  const y = d.getFullYear();
  const m = String(d.getMonth() + 1).padStart(2, "0");
  const day = String(d.getDate()).padStart(2, "0");
  return `${y}-${m}-${day}`;
}

function formatBytes(n: number): string {
  if (n <= 0) return "—";
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(0)} Ko`;
  return `${(n / (1024 * 1024)).toFixed(1)} Mo`;
}

export function FlatFilesModal({ open, onClose }: Props) {
  const { data: config } = useLocalConfig();
  const update = useUpdateLocalConfig();
  const mode = config?.data_source?.mode ?? "api";

  // Download range (defaults: last 30 days → yesterday).
  const today = useMemo(() => new Date(), []);
  const [start, setStart] = useState<string>(() => {
    const d = new Date();
    d.setDate(d.getDate() - 30);
    return ymd(d);
  });
  const [end, setEnd] = useState<string>(() => {
    const d = new Date();
    d.setDate(d.getDate() - 1);
    return ymd(d);
  });
  const [startErr, setStartErr] = useState<string | null>(null);

  // Calendar month being viewed.
  const [month, setMonth] = useState<Date>(() => new Date(today.getFullYear(), today.getMonth(), 1));

  // Download status (polled while the modal is open).
  const [status, setStatus] = useState<FlatFilesStatus | null>(null);
  useEffect(() => {
    if (!open) return;
    let active = true;
    const tick = () => api.getFlatFilesStatus().then((s) => active && setStatus(s)).catch(() => {});
    tick();
    const id = window.setInterval(tick, 700);
    return () => {
      active = false;
      window.clearInterval(id);
    };
  }, [open]);

  // Days available on disk → calendar colouring. Refetched as days complete.
  const calendar = useQuery({
    queryKey: ["flat-files-calendar"],
    queryFn: api.getFlatFilesCalendar,
    enabled: open,
  });
  useEffect(() => {
    if (open) calendar.refetch();
    // Refetch whenever a new day finishes downloading.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [status?.last_done, open]);

  const byDay = useMemo(() => {
    const m = new Map<string, FlatFileDay>();
    for (const d of calendar.data ?? []) m.set(d.day, d);
    return m;
  }, [calendar.data]);

  const running = status?.running ?? false;

  const setMode = (next: "api" | "flat_files") => {
    if (!config || next === mode) return;
    update.mutate({ ...config, data_source: { mode: next } });
  };

  const startDownload = async () => {
    setStartErr(null);
    try {
      await api.flatFilesDownload(start, end);
    } catch (e) {
      setStartErr(String(e));
    }
  };

  // ── Calendar grid (Monday-first) ──
  const grid = useMemo(() => {
    const first = new Date(month.getFullYear(), month.getMonth(), 1);
    const daysInMonth = new Date(month.getFullYear(), month.getMonth() + 1, 0).getDate();
    // JS getDay: 0=Sun..6=Sat → Monday-first offset.
    const lead = (first.getDay() + 6) % 7;
    const cells: (Date | null)[] = [];
    for (let i = 0; i < lead; i++) cells.push(null);
    for (let d = 1; d <= daysInMonth; d++) cells.push(new Date(month.getFullYear(), month.getMonth(), d));
    while (cells.length % 7 !== 0) cells.push(null);
    return cells;
  }, [month]);

  const todayKey = ymd(today);

  const availableCount = (calendar.data ?? []).filter((d) => d.complete).length;

  return (
    <Dialog open={open} onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="max-w-2xl">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <Database className="h-4 w-4" />
            Gestion Flat Files
          </DialogTitle>
        </DialogHeader>

        <div className="space-y-4">
          {/* ── Data source toggle (duplicated in Settings → API Keys) ── */}
          <div className="rounded-md border border-border p-3">
            <div className="flex items-center justify-between gap-4">
              <div className="min-w-0">
                <div className="text-sm font-medium">Source de données</div>
                <p className="mt-0.5 text-xs text-muted-foreground">
                  {mode === "flat_files"
                    ? "Flat files — aucune donnée temps réel. Market Replay uniquement (ouvert par défaut au démarrage)."
                    : "API Alpaca — données temps réel (trading en direct possible)."}
                </p>
              </div>
              <div className="flex shrink-0 overflow-hidden rounded-md border border-border">
                {(["api", "flat_files"] as const).map((m) => (
                  <button
                    key={m}
                    onClick={() => setMode(m)}
                    disabled={update.isPending}
                    className={cn(
                      "flex items-center gap-1.5 px-3 py-1.5 text-xs transition-colors",
                      mode === m
                        ? "bg-accent text-foreground"
                        : "text-muted-foreground hover:bg-accent/50",
                    )}
                  >
                    {m === "api" ? <Radio className="h-3.5 w-3.5" /> : <Database className="h-3.5 w-3.5" />}
                    {m === "api" ? "API" : "Flat files"}
                  </button>
                ))}
              </div>
            </div>
          </div>

          {/* ── Download a date range ── */}
          <div className="rounded-md border border-border p-3">
            <div className="mb-2 text-sm font-medium">Télécharger des journées</div>
            <p className="mb-3 text-xs text-muted-foreground">
              Barres 1 minute (pré-marché + séance + post-marché) du sous-ensemble liquide
              de l'univers US, un fichier SQLite par jour. Les week-ends et jours déjà
              téléchargés sont ignorés.
            </p>
            <div className="flex flex-wrap items-end gap-3">
              <label className="flex flex-col gap-1 text-[11px] text-muted-foreground">
                Du
                <input
                  type="date"
                  value={start}
                  max={end}
                  onChange={(e) => setStart(e.target.value)}
                  className="h-7 rounded border border-border bg-background px-2 text-xs text-foreground"
                />
              </label>
              <label className="flex flex-col gap-1 text-[11px] text-muted-foreground">
                Au
                <input
                  type="date"
                  value={end}
                  min={start}
                  max={todayKey}
                  onChange={(e) => setEnd(e.target.value)}
                  className="h-7 rounded border border-border bg-background px-2 text-xs text-foreground"
                />
              </label>

              {running ? (
                <Button size="sm" variant="destructive" onClick={() => api.flatFilesCancel().catch(() => {})}>
                  <Square className="mr-1.5 h-3.5 w-3.5" /> Annuler
                </Button>
              ) : (
                <Button size="sm" onClick={startDownload}>
                  <Download className="mr-1.5 h-3.5 w-3.5" /> Télécharger
                </Button>
              )}

              <Button size="sm" variant="outline" onClick={() => api.openFlatFilesFolder().catch(() => {})}>
                <FolderOpen className="mr-1.5 h-3.5 w-3.5" /> Ouvrir le dossier
              </Button>
            </div>

            {running && (
              <div className="mt-3">
                <div className="flex items-center justify-between text-xs text-muted-foreground">
                  <span className="flex items-center gap-1.5">
                    <Loader2 className="h-3.5 w-3.5 animate-spin" />
                    {status?.current_day ?? "…"} · jour {status?.day_index ?? 0}/{status?.day_total ?? 0}
                  </span>
                  <span className="tabular-nums">{Math.round((status?.progress ?? 0) * 100)} %</span>
                </div>
                <div className="mt-1 h-1.5 w-full overflow-hidden rounded bg-muted">
                  <div
                    className="h-full bg-blue-500 transition-all"
                    style={{
                      width: `${
                        status && status.day_total > 0
                          ? Math.round(((status.day_index - 1 + status.progress) / status.day_total) * 100)
                          : 0
                      }%`,
                    }}
                  />
                </div>
              </div>
            )}
            {startErr && <p className="mt-2 text-xs text-red-400">{startErr}</p>}
            {!running && status?.state === "done" && status?.error && (
              <p className="mt-2 text-xs text-amber-400" title={status.error}>
                Terminé avec des jours ignorés (jours fériés / sans données).
              </p>
            )}
            <p className="mt-2 text-[11px] text-muted-foreground">
              Pour <strong>importer</strong> les flat files d'un autre utilisateur TagDash&nbsp;:
              ouvrez le dossier et déposez-y ses fichiers <code className="rounded bg-muted px-1">flat-*.db</code> —
              ils apparaissent en vert dans le calendrier et deviennent rejouables.
            </p>
          </div>

          {/* ── Calendar ── */}
          <div className="rounded-md border border-border p-3">
            <div className="mb-2 flex items-center justify-between">
              <div className="text-sm font-medium">
                Jours disponibles{" "}
                <span className="text-xs font-normal text-muted-foreground">
                  ({availableCount} sur disque)
                </span>
              </div>
              <div className="flex items-center gap-1">
                <button
                  onClick={() => setMonth((m) => new Date(m.getFullYear(), m.getMonth() - 1, 1))}
                  className="flex h-6 w-6 items-center justify-center rounded text-muted-foreground hover:bg-accent hover:text-foreground"
                >
                  <ChevronLeft className="h-4 w-4" />
                </button>
                <span className="min-w-[8.5rem] text-center text-xs font-medium">
                  {MONTH_LABELS[month.getMonth()]} {month.getFullYear()}
                </span>
                <button
                  onClick={() => setMonth((m) => new Date(m.getFullYear(), m.getMonth() + 1, 1))}
                  className="flex h-6 w-6 items-center justify-center rounded text-muted-foreground hover:bg-accent hover:text-foreground"
                >
                  <ChevronRight className="h-4 w-4" />
                </button>
              </div>
            </div>

            <div className="grid grid-cols-7 gap-1">
              {WEEKDAY_LABELS.map((w) => (
                <div key={w} className="py-1 text-center text-[10px] uppercase text-muted-foreground/60">
                  {w}
                </div>
              ))}
              {grid.map((d, i) => {
                if (!d) return <div key={i} />;
                const key = ymd(d);
                const entry = byDay.get(key);
                const weekend = d.getDay() === 0 || d.getDay() === 6;
                const future = key > todayKey;
                const isToday = key === todayKey;
                const color = entry
                  ? entry.complete
                    ? "bg-emerald-600/30 text-emerald-200 border-emerald-600/50"
                    : "bg-amber-600/30 text-amber-200 border-amber-600/50"
                  : weekend
                    ? "bg-transparent text-muted-foreground/30 border-transparent"
                    : future
                      ? "bg-transparent text-muted-foreground/30 border-transparent"
                      : "bg-muted/40 text-muted-foreground border-border/40";
                const title = entry
                  ? `${key} — ${entry.complete ? "disponible" : "partiel"} · ${entry.symbol_count} symboles · ${entry.bar_count.toLocaleString()} barres · ${formatBytes(entry.bytes)}`
                  : weekend
                    ? `${key} — week-end`
                    : future
                      ? `${key} — à venir`
                      : `${key} — non téléchargé`;
                return (
                  <div
                    key={i}
                    title={title}
                    className={cn(
                      "flex h-9 flex-col items-center justify-center rounded border text-xs tabular-nums",
                      color,
                      isToday && "ring-1 ring-blue-400/70",
                    )}
                  >
                    {d.getDate()}
                  </div>
                );
              })}
            </div>

            <div className="mt-3 flex flex-wrap items-center gap-x-4 gap-y-1 text-[10px] text-muted-foreground">
              <span className="flex items-center gap-1">
                <span className="h-2.5 w-2.5 rounded-sm border border-emerald-600/50 bg-emerald-600/30" /> disponible
              </span>
              <span className="flex items-center gap-1">
                <span className="h-2.5 w-2.5 rounded-sm border border-amber-600/50 bg-amber-600/30" /> partiel
              </span>
              <span className="flex items-center gap-1">
                <span className="h-2.5 w-2.5 rounded-sm border border-border/40 bg-muted/40" /> non téléchargé
              </span>
            </div>
          </div>
        </div>

        <div className="mt-2 flex justify-end">
          <Button variant="ghost" size="sm" onClick={onClose}>
            Fermer
          </Button>
        </div>
      </DialogContent>
    </Dialog>
  );
}
