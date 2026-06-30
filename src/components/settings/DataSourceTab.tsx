// Settings → Flux de données → Source de données. Ported from the former FlatFilesModal —
// download, browse and share offline market-data days, plus the API ↔ Flat files toggle
// (now defined here only — it used to be duplicated in Settings → API Keys).
//
// Three datasets live side by side, each with its own download logic + calendar:
//   • Trade  — real trades + quotes inside the [alert−1min, +10min] windows where a
//     minute pre-scan says Micro Pullback would ignite (historical float aware).
//   • Minute — 1-minute bars of the day's 2000 most-traded symbols (04:00→20:00 ET),
//     plus the 5 previous trading days for intraday chart history.
//   • Daily  — daily bars of the whole US universe, appended into a cumulative
//     daily.db whose table is identical to the local database.

import { useEffect, useMemo, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import {
  ChevronLeft,
  ChevronRight,
  Clock,
  Database,
  Download,
  FolderOpen,
  Loader2,
  Radio,
  Square,
} from "lucide-react";
import { Button } from "@/components/ui/button";
import { RestartRequiredDialog } from "@/components/RestartRequiredDialog";
import { api } from "@/lib/tauri";
import { cn } from "@/lib/utils";
import { useLocalConfig, useUpdateLocalConfig } from "@/queries/useLocalConfig";
import type { FlatFileDay, FlatFilesKind, FlatFilesStatus } from "@/types";

const WEEKDAY_LABELS = ["Lun", "Mar", "Mer", "Jeu", "Ven", "Sam", "Dim"];
const MONTH_LABELS = [
  "Janvier", "Février", "Mars", "Avril", "Mai", "Juin",
  "Juillet", "Août", "Septembre", "Octobre", "Novembre", "Décembre",
];

const KIND_META: Record<
  FlatFilesKind,
  { label: string; blurb: string; unitSymbols: string; unitBars: string }
> = {
  trade: {
    label: "Trade",
    blurb:
      "Trades + quotes réels autour des fenêtres où Micro Pullback se déclencherait " +
      "(pré-scan minute), de −1 min à +10 min. Le float historique du jour est pris en " +
      "compte. Un fichier SQLite par jour.",
    unitSymbols: "tickers",
    unitBars: "trades",
  },
  minute: {
    label: "Minute",
    blurb:
      "Barres 1 minute (pré-marché + séance + post-marché) des 2000 actions les plus " +
      "échangées du jour, plus les 5 jours précédents pour l'historique intraday. " +
      "Un fichier par jour.",
    unitSymbols: "symboles",
    unitBars: "barres",
  },
  daily: {
    label: "Daily",
    blurb:
      "Barres journalières (daily) de tout l'univers US, ajoutées dans une base " +
      "cumulative daily.db au format identique à la base de données.",
    unitSymbols: "symboles",
    unitBars: "barres",
  },
};

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

export function DataSourceTab() {
  const { data: config } = useLocalConfig();
  const update = useUpdateLocalConfig();
  const mode = config?.data_source?.mode ?? "api";

  // Switching data source changes the startup pipeline → prompt for a restart.
  const [showRestart, setShowRestart] = useState(false);

  // Active sub-tab (dataset).
  const [tab, setTab] = useState<FlatFilesKind>("minute");

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

  // Download status (polled while the tab is mounted).
  const [status, setStatus] = useState<FlatFilesStatus | null>(null);
  useEffect(() => {
    let active = true;
    const tick = () => api.getFlatFilesStatus().then((s) => active && setStatus(s)).catch(() => {});
    tick();
    const id = window.setInterval(tick, 700);
    return () => {
      active = false;
      window.clearInterval(id);
    };
  }, []);

  // Days available on disk for the active tab → calendar colouring.
  const calendar = useQuery({
    queryKey: ["flat-files-calendar", tab],
    queryFn: () => api.getFlatFilesCalendar(tab),
  });
  useEffect(() => {
    calendar.refetch();
    // Refetch whenever a new day finishes downloading or the tab changes.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [status?.last_done, tab]);

  const byDay = useMemo(() => {
    const m = new Map<string, FlatFileDay>();
    for (const d of calendar.data ?? []) m.set(d.day, d);
    return m;
  }, [calendar.data]);

  const running = status?.running ?? false;
  const meta = KIND_META[tab];

  const setMode = (next: "api" | "flat_files") => {
    if (!config || next === mode) return;
    update.mutate(
      { ...config, data_source: { mode: next } },
      { onSuccess: () => setShowRestart(true) },
    );
  };

  const startDownload = async () => {
    setStartErr(null);
    try {
      await api.flatFilesDownload(tab, start, end);
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

  // Daily coverage (first/last date present in the cumulative file).
  const coverage = useMemo(() => {
    const days = (calendar.data ?? []).map((d) => d.day).sort();
    if (days.length === 0) return null;
    return { from: days[0], to: days[days.length - 1], count: days.length };
  }, [calendar.data]);

  return (
    <>
      <div className="space-y-4">
        {/* ── Data source toggle ── */}
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

        {/* ── Sub-tabs: Trade / Minute / Daily ── */}
        <div className="flex overflow-hidden rounded-md border border-border">
          {(Object.keys(KIND_META) as FlatFilesKind[]).map((k) => (
            <button
              key={k}
              onClick={() => setTab(k)}
              className={cn(
                "flex-1 px-3 py-2 text-xs font-medium transition-colors",
                tab === k ? "bg-accent text-foreground" : "text-muted-foreground hover:bg-accent/50",
              )}
            >
              {KIND_META[k].label}
              {running && status?.kind === k && (
                <Loader2 className="ml-1.5 inline h-3 w-3 animate-spin" />
              )}
            </button>
          ))}
        </div>

        {/* ── Download a date range ── */}
        <div className="rounded-md border border-border p-3">
          <div className="mb-2 text-sm font-medium">
            {tab === "daily" ? "Télécharger une plage daily" : "Télécharger des journées"}
          </div>
          <p className="mb-3 text-xs text-muted-foreground">{meta.blurb}</p>
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

            <Button size="sm" variant="outline" onClick={() => api.openFlatFilesFolder(tab).catch(() => {})}>
              <FolderOpen className="mr-1.5 h-3.5 w-3.5" /> Ouvrir le dossier
            </Button>
          </div>

          {running && (
            <div className="mt-3">
              <div className="flex items-center justify-between text-xs text-muted-foreground">
                <span className="flex items-center gap-1.5">
                  <Loader2 className="h-3.5 w-3.5 animate-spin" />
                  {status?.kind ? `[${status.kind}] ` : ""}
                  {status?.current_day ?? "…"} · {status?.day_index ?? 0}/{status?.day_total ?? 0}
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
          {!running && status?.state === "error" && status?.error && (
            <p className="mt-2 text-xs text-red-400" title={status.error}>
              Échec : {status.error}
            </p>
          )}
          {!running && status?.state === "done" && status?.error && (
            <p className="mt-2 text-xs text-amber-400" title={status.error}>
              Terminé avec des jours ignorés (jours fériés / sans données).
            </p>
          )}
          <p className="mt-2 text-[11px] text-muted-foreground">
            Pour <strong>importer</strong> les flat files d'un autre utilisateur TagDash&nbsp;:
            ouvrez le dossier et déposez-y ses fichiers — ils apparaissent en vert dans le
            calendrier et deviennent rejouables.
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

          {tab === "daily" && coverage && (
            <div className="mb-2 flex items-center gap-1.5 text-[11px] text-muted-foreground">
              <Clock className="h-3 w-3" />
              Couverture daily.db : {coverage.from} → {coverage.to} ({coverage.count} jours)
            </div>
          )}

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
                ? `${key} — ${entry.complete ? "disponible" : "partiel"} · ${entry.symbol_count} ${meta.unitSymbols} · ${entry.bar_count.toLocaleString()} ${meta.unitBars}${entry.bytes > 0 ? ` · ${formatBytes(entry.bytes)}` : ""}`
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
      <RestartRequiredDialog open={showRestart} onClose={() => setShowRestart(false)} />
    </>
  );
}
