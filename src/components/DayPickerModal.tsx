// « Préparer une séance » — choose the strategies, then a day, then launch.
//
// A replay reads only what was prepared on disk for the chosen strategies: real
// SIP prints and quotes around their confirmed opportunities, synthetic 10-second
// slices elsewhere. The calendar therefore shows, per day and FOR THESE
// STRATEGIES WITH THEIR CURRENT SETTINGS, whether the day is ready, needs
// completing (settings changed, ticks missing) or was never prepared — and
// preparing downloads only what is missing.
//
// A day does not have to be finished to be replayed: its opportunities are
// confirmed in time order, each with its ticks, so the session can start on the
// first ones while the rest downloads behind it (`launchable`). One day asked for
// here goes ahead of a month queued in the background.

import { useCallback, useEffect, useMemo, useState } from "react";
import {
  CalendarDays,
  Check,
  ChevronLeft,
  ChevronRight,
  CircleDashed,
  CircleDot,
  Download,
  FolderOpen,
  Loader2,
  Play,
  Square,
} from "lucide-react";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { api } from "@/lib/tauri";
import { nyTime } from "@/lib/nyTime";
import { cn } from "@/lib/utils";
import type {
  EtWindow,
  ReplayDataBudget,
  ReplayDayState,
  ReplayDayStatus,
  ReplayPrepareStatus,
  ReplayStrategy,
} from "@/types";

interface Props {
  open: boolean;
  onClose: () => void;
  /** A day that can be replayed now, the suggested start (first session start, ET
   *  "HH:MM") and the strategies it was prepared for. */
  onLaunch: (day: string, startHm: string, strategies: string[]) => void;
}

const WEEKDAY_LABELS = ["Lu", "Ma", "Me", "Je", "Ve"];

const STAGES: Record<string, string> = {
  market: "Instantané du marché",
  minutes: "Barres 1 min des candidats",
  scan: "Confirmation sur prints, dans l'ordre de la séance",
  ticks: "Prints et quotes des opportunités",
  news: "News",
};

const PLANS: Record<ReplayDataBudget["plan"], string> = {
  iex: "Alpaca Basic",
  sip: "Alpaca Algo Trader Plus",
};

const STATE_META: Record<ReplayDayState, { label: string; hint: string; cls: string }> = {
  ready: {
    label: "Prêt",
    hint: "Tout ce que ces stratégies demandent est sur disque",
    cls: "text-emerald-300",
  },
  incomplete: {
    label: "À compléter",
    hint: "Réglages modifiés depuis la préparation, ou ticks manquants — seul le complément sera téléchargé",
    cls: "text-amber-300",
  },
  not_prepared: {
    label: "Non préparé",
    hint: "Rien sur disque pour ce jour",
    cls: "text-muted-foreground",
  },
  unavailable: {
    label: "Indisponible",
    hint: "Week-end, jour férié, ou séance pas encore servie en SIP historique (clôture + 15 min)",
    cls: "text-muted-foreground/40",
  },
};

const hm = (m: number) =>
  `${String(Math.floor(m / 60)).padStart(2, "0")}:${String(m % 60).padStart(2, "0")}`;
const windowLabel = (ws: EtWindow[]) =>
  ws.map((w) => `${hm(w.start_min)}–${hm(w.end_min)}`).join(", ");

function mb(bytes: number): string {
  if (bytes < 1_000_000) return `${Math.max(1, Math.round(bytes / 1_000))} Ko`;
  return `${(bytes / 1_000_000).toFixed(bytes < 10_000_000 ? 1 : 0)} Mo`;
}

function eta(secs: number): string {
  const min = Math.max(1, Math.round(secs / 60));
  if (min < 90) return `≈ ${min} min`;
  return `≈ ${Math.floor(min / 60)} h ${String(min % 60).padStart(2, "0")}`;
}

function monthKey(year: number, month: number) {
  return `${year}-${String(month + 1).padStart(2, "0")}`;
}

/** Weekdays of the month as a Mon–Fri grid (leading blanks for the first week). */
function weekdayCells(year: number, month: number): (string | null)[] {
  const cells: (string | null)[] = [];
  const first = new Date(year, month, 1).getDay(); // 0 = Sun
  const lead = first === 0 || first === 6 ? 0 : first - 1;
  for (let i = 0; i < lead; i++) cells.push(null);
  const days = new Date(year, month + 1, 0).getDate();
  for (let d = 1; d <= days; d++) {
    const dow = new Date(year, month, d).getDay();
    if (dow === 0 || dow === 6) continue;
    cells.push(`${monthKey(year, month)}-${String(d).padStart(2, "0")}`);
  }
  return cells;
}

const hitsOf = (s: ReplayDayStatus) => s.strategies.reduce((n, x) => n + x.hits, 0);

function StateIcon({ state, className }: { state: ReplayDayState; className?: string }) {
  if (state === "ready") return <Check className={cn("h-3 w-3 text-emerald-400", className)} />;
  if (state === "incomplete") return <CircleDot className={cn("h-3 w-3 text-amber-400", className)} />;
  if (state === "not_prepared")
    return <CircleDashed className={cn("h-3 w-3 text-muted-foreground/50", className)} />;
  return null;
}

export function DayPickerModal({ open, onClose, onLaunch }: Props) {
  const [strategies, setStrategies] = useState<ReplayStrategy[]>([]);
  const [chosen, setChosen] = useState<string[]>([]);
  const now = new Date();
  const [viewYear, setViewYear] = useState(now.getFullYear());
  const [viewMonth, setViewMonth] = useState(now.getMonth());
  const [calendar, setCalendar] = useState<ReplayDayStatus[]>([]);
  const [day, setDay] = useState<string | null>(null);
  const [detail, setDetail] = useState<ReplayDayStatus | null>(null);
  const [prep, setPrep] = useState<ReplayPrepareStatus | null>(null);
  const [budget, setBudget] = useState<ReplayDataBudget | null>(null);
  const [err, setErr] = useState<string | null>(null);

  useEffect(() => {
    if (!open) return;
    api.replayDataBudget().then(setBudget).catch(() => {});
    api
      .replayDataStrategies()
      .then((list) => {
        setStrategies(list);
        setChosen((c) => (c.length ? c.filter((id) => list.some((s) => s.id === id)) : list.map((s) => s.id)));
      })
      .catch((e) => setErr(String(e)));
  }, [open]);

  const month = monthKey(viewYear, viewMonth);
  const refresh = useCallback(() => {
    if (!open || chosen.length === 0) return;
    api.replayDataCalendar(month, chosen).then(setCalendar).catch((e) => setErr(String(e)));
    if (day) api.replayDataDay(day, chosen).then(setDetail).catch(() => {});
  }, [open, month, chosen, day]);
  useEffect(refresh, [refresh]);

  // Preparation progress: polled while the modal is open. The calendar and the day
  // are refreshed as a run goes (opportunities appear one by one, and the day
  // becomes launchable long before it is finished) and when it ends.
  useEffect(() => {
    if (!open) return;
    let wasRunning = false;
    let polls = 0;
    const tick = () =>
      api
        .replayDataStatus()
        .then((s) => {
          setPrep(s);
          polls += 1;
          if ((wasRunning && !s.running) || (s.running && polls % 3 === 0)) refresh();
          wasRunning = s.running;
        })
        .catch(() => {});
    tick();
    const id = window.setInterval(tick, 700);
    return () => window.clearInterval(id);
  }, [open, refresh]);

  const byDay = useMemo(() => new Map(calendar.map((s) => [s.day, s])), [calendar]);
  const running = prep?.running ?? false;
  const selected = strategies.filter((s) => chosen.includes(s.id));
  const firstStart = selected.length
    ? hm(Math.min(...selected.flatMap((s) => s.sessions.map((w) => w.start_min))))
    : "04:00";

  const toggle = (id: string) =>
    setChosen((c) => (c.includes(id) ? c.filter((x) => x !== id) : [...c, id]));

  // `priority`: the day is wanted now — it goes ahead of whatever is queued, and
  // a day already downloading steps aside for it.
  const prepare = async (days: string[], scanOnly: boolean, priority: boolean) => {
    setErr(null);
    try {
      await api.replayDataPrepare(days, chosen, scanOnly, priority);
      api.replayDataStatus().then(setPrep).catch(() => {});
      api.replayDataBudget().then(setBudget).catch(() => {});
    } catch (e) {
      setErr(String(e));
    }
  };

  const prev = () => {
    if (viewMonth === 0) { setViewYear((y) => y - 1); setViewMonth(11); }
    else setViewMonth((m) => m - 1);
  };
  const next = () => {
    if (viewMonth === 11) { setViewYear((y) => y + 1); setViewMonth(0); }
    else setViewMonth((m) => m + 1);
  };

  // The month's days still to prepare — those the trader took trades on first:
  // they are the ones most likely to be replayed.
  const pendingInMonth = calendar
    .filter((s) => s.state === "not_prepared" || s.state === "incomplete")
    .sort((a, b) => Number(b.trades > 0) - Number(a.trades > 0) || a.day.localeCompare(b.day));
  const d = detail && detail.day === day ? detail : null;
  const scannedAll = d?.strategies.every((s) => s.scanned) ?? false;
  // This day is downloading, or waiting its turn.
  const dayPending = !!day && running && (prep?.day === day || (prep?.queue ?? []).includes(day));
  const offInLive = selected.filter((s) => !s.enabled);
  const launch = (startHm: string) => {
    if (!day) return;
    onLaunch(day, startHm, chosen);
    onClose();
  };
  // An opportunity can be started on once the day is launchable and, on a day
  // still downloading, prepared at least up to its lead-in.
  const startable = (alertMs: number) =>
    !!d?.launchable && (d.ready_until_ms == null || alertMs - 120_000 <= d.ready_until_ms);

  return (
    <Dialog open={open} onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="max-h-[90vh] max-w-3xl overflow-y-auto">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <CalendarDays className="h-4 w-4" />
            Préparer une séance
          </DialogTitle>
        </DialogHeader>

        {/* 1 — Strategies. Everything below is computed for THESE, with their
            current settings. */}
        <div className="space-y-1.5">
          <div className="text-[11px] font-medium">Stratégies</div>
          <div className="flex flex-wrap gap-1.5">
            {strategies.map((s) => (
              <button
                key={s.id}
                onClick={() => toggle(s.id)}
                disabled={running}
                className={cn(
                  "rounded border px-2 py-1 text-[11px] transition-colors",
                  chosen.includes(s.id)
                    ? "border-violet-500/50 bg-violet-500/15 text-foreground"
                    : "border-border text-muted-foreground hover:bg-accent/50",
                )}
              >
                {s.label}
                <span className="ml-1.5 tabular-nums text-muted-foreground">
                  {windowLabel(s.sessions)} ET
                </span>
              </button>
            ))}
          </div>
          <p className="text-[10px] leading-snug text-muted-foreground">
            Seules les données dont ces stratégies ont besoin sont téléchargées (Alpaca
            SIP) : les minutes des titres de leur univers, puis les vrais prints et quotes
            autour de chaque opportunité confirmée. Le créneau et les seuils se règlent dans
            Réglages → Stratégies ; les changer marque les jours préparés « à compléter ».
          </p>
          {offInLive.length > 0 && (
            <p className="text-[10px] leading-snug text-amber-300/90">
              {offInLive.map((s) => s.label).join(", ")} : désactivée en live (Réglages →
              Stratégies). Elle tourne quand même pendant un replay préparé pour elle, sans
              toucher à ce réglage.
            </p>
          )}
        </div>

        <div className="grid gap-4 md:grid-cols-[minmax(0,17rem)_minmax(0,1fr)]">
          {/* 2 — Calendar */}
          <div className="space-y-2">
            <div className="flex items-center justify-between">
              <button onClick={prev} className="rounded p-1 hover:bg-accent" aria-label="Mois précédent">
                <ChevronLeft className="h-4 w-4" />
              </button>
              <span className="text-sm font-medium capitalize">
                {new Date(viewYear, viewMonth).toLocaleDateString("fr-FR", {
                  month: "long",
                  year: "numeric",
                })}
              </span>
              <button onClick={next} className="rounded p-1 hover:bg-accent" aria-label="Mois suivant">
                <ChevronRight className="h-4 w-4" />
              </button>
            </div>
            <div className="grid grid-cols-5 gap-px text-center text-[10px] text-muted-foreground">
              {WEEKDAY_LABELS.map((w) => (
                <div key={w} className="py-0.5">{w}</div>
              ))}
            </div>
            <div className="grid grid-cols-5 gap-px">
              {weekdayCells(viewYear, viewMonth).map((cell, i) => {
                if (!cell) return <div key={`e${i}`} />;
                const s = byDay.get(cell);
                const state: ReplayDayState = s?.state ?? "unavailable";
                const hits = s ? hitsOf(s) : 0;
                const trades = s?.trades ?? 0;
                const disabled = state === "unavailable";
                return (
                  <button
                    key={cell}
                    disabled={disabled}
                    onClick={() => setDay(cell)}
                    title={`${STATE_META[state].label} — ${STATE_META[state].hint}${
                      hits ? ` · ${hits} opportunité${hits > 1 ? "s" : ""}` : ""
                    }${trades ? ` · ${trades} trade${trades > 1 ? "s" : ""} pris ce jour-là` : ""}`}
                    className={cn(
                      "relative flex h-10 flex-col items-center justify-center rounded text-xs transition-colors",
                      disabled ? "cursor-default text-muted-foreground/30" : "hover:bg-accent",
                      day === cell && "bg-accent ring-1 ring-violet-500/60",
                      prep?.running && prep.day === cell && "animate-pulse",
                    )}
                  >
                    {trades > 0 && (
                      <span className="absolute right-1 top-1 h-1.5 w-1.5 rounded-full bg-sky-400" />
                    )}
                    <span className={STATE_META[state].cls}>{parseInt(cell.slice(8), 10)}</span>
                    <span className="flex h-3 items-center gap-0.5 text-[9px] tabular-nums text-muted-foreground">
                      <StateIcon state={state} className="h-2.5 w-2.5" />
                      {hits > 0 && hits}
                    </span>
                  </button>
                );
              })}
            </div>
            <div className="flex flex-wrap gap-x-3 gap-y-1 border-t border-border pt-2 text-[10px] text-muted-foreground">
              {(["ready", "incomplete", "not_prepared"] as const).map((k) => (
                <span key={k} className="flex items-center gap-1" title={STATE_META[k].hint}>
                  <StateIcon state={k} /> {STATE_META[k].label}
                </span>
              ))}
              <span>· chiffre = opportunités</span>
              <span
                className="flex items-center gap-1"
                title="Des trades ont été pris ce jour-là sur un ticker du scanner de ces stratégies (Apex et Dip comptent pour Micro Pullback)"
              >
                <span className="h-1.5 w-1.5 rounded-full bg-sky-400" /> trades pris
              </span>
            </div>
            {pendingInMonth.length > 0 && (
              <button
                disabled={chosen.length === 0}
                onClick={() => prepare(pendingInMonth.map((s) => s.day), false, false)}
                title="Télécharge tous les jours du mois en arrière-plan, ceux où des trades ont été pris d'abord. La fenêtre peut être fermée et un replay lancé pendant ce temps."
                className="flex w-full items-center justify-center gap-1.5 rounded border border-border py-1 text-[11px] text-muted-foreground hover:bg-accent hover:text-foreground disabled:opacity-40"
              >
                <Download className="h-3.5 w-3.5" />
                Préparer le mois ({pendingInMonth.length} jour{pendingInMonth.length > 1 ? "s" : ""})
              </button>
            )}
            <p className="text-[10px] leading-snug text-muted-foreground">
              {budget
                ? `${PLANS[budget.plan]} · ${budget.requests_per_minute.toLocaleString("fr-FR")} requêtes/min — le téléchargement suit ce rythme.`
                : "Le téléchargement suit le rythme du forfait Alpaca."}
              {running && prep && prep.day_total > 1 && (
                <>
                  {" "}
                  File : jour {Math.min(prep.day_index + 1, prep.day_total)}/{prep.day_total}
                  {prep.eta_secs != null && ` · ${eta(prep.eta_secs)} restantes`}.
                </>
              )}
            </p>
          </div>

          {/* 3 — The selected day */}
          <div className="min-w-0 space-y-2 rounded border border-border/60 p-2.5">
            {!day && (
              <p className="text-[11px] text-muted-foreground">
                Choisissez un jour pour voir ses opportunités et ce qu'il reste à télécharger.
              </p>
            )}
            {day && (
              <>
                <div className="flex items-baseline justify-between gap-2">
                  <span className="text-sm font-medium">
                    {new Date(`${day}T12:00:00`).toLocaleDateString("fr-FR", {
                      weekday: "long",
                      day: "numeric",
                      month: "long",
                    })}
                  </span>
                  {d && (
                    <span className={cn("flex items-center gap-1 text-[11px]", STATE_META[d.state].cls)}>
                      <StateIcon state={d.state} /> {STATE_META[d.state].label}
                    </span>
                  )}
                </div>

                {d && d.state !== "not_prepared" && d.state !== "unavailable" && (
                  <div className="text-[10px] text-muted-foreground">
                    {d.strategies
                      .map((s) => {
                        const label = strategies.find((x) => x.id === s.strategy_id)?.label ?? s.strategy_id;
                        if (s.scanned)
                          return `${label} : ${s.hits} opportunité${s.hits > 1 ? "s" : ""} sur ${s.symbols} ticker${s.symbols > 1 ? "s" : ""}`;
                        return s.partial
                          ? `${label} : ${s.hits} opportunité${s.hits > 1 ? "s" : ""} trouvée${s.hits > 1 ? "s" : ""}, confirmation en cours`
                          : `${label} : réglages modifiés depuis la préparation`;
                      })
                      .join(" · ")}
                    {" · "}
                    {mb(d.bytes_on_disk)} sur disque
                  </div>
                )}

                {d && d.state !== "ready" && d.launchable && (
                  <p className="text-[10px] leading-snug text-emerald-300/90">
                    Début de séance prêt ({d.ready_hits} opportunité{d.ready_hits > 1 ? "s" : ""} avec
                    leurs ticks) : le replay peut démarrer, le reste du jour se télécharge
                    pendant la séance.
                  </p>
                )}

                {d && d.opportunities.length > 0 && (
                  <div className="max-h-56 overflow-y-auto rounded bg-background/40">
                    <table className="w-full text-[11px]">
                      <tbody>
                        {d.opportunities.map((o, i) => (
                          <tr
                            key={i}
                            onClick={() => {
                              if (!startable(o.alert_ms)) return;
                              // Two minutes of lead-in: the ignition itself is on screen.
                              launch(nyTime(new Date(o.alert_ms - 120_000).toISOString(), false));
                            }}
                            title={startable(o.alert_ms) ? "Lancer le replay 2 min avant cette opportunité" : o.reason}
                            className={cn(
                              "border-b border-border/30 last:border-0",
                              startable(o.alert_ms) && "cursor-pointer hover:bg-accent/60",
                            )}
                          >
                            <td className="px-1.5 py-0.5 font-mono tabular-nums text-muted-foreground">
                              {nyTime(new Date(o.alert_ms).toISOString(), false)}
                            </td>
                            <td className="px-1.5 py-0.5 font-medium">{o.symbol}</td>
                            <td className="truncate px-1.5 py-0.5 text-muted-foreground" title={o.reason}>
                              {o.reason}
                            </td>
                          </tr>
                        ))}
                      </tbody>
                    </table>
                  </div>
                )}

                {d && d.state === "incomplete" && scannedAll && d.missing_ms > 0 && (
                  <p className="text-[10px] text-muted-foreground">
                    À télécharger : {Math.round(d.missing_ms / 60_000)} min de ticks ·
                    ≈ {mb(d.estimate_bytes)} · {d.estimate_requests} requête{d.estimate_requests > 1 ? "s" : ""}
                  </p>
                )}
                {d && (d.state === "not_prepared" || !scannedAll) && !d.launchable && !dayPending && (
                  <p className="text-[10px] leading-snug text-muted-foreground">
                    Préparation : minutes de l'univers des stratégies, pré-scan, puis
                    confirmation sur les vrais prints dans l'ordre de la séance, chaque
                    opportunité avec ses ticks. Le replay peut démarrer dès les deux premières
                    (une à deux minutes) ; le reste du jour suit en arrière-plan.
                  </p>
                )}

                {running && (
                  <div className="space-y-1 rounded bg-background/40 p-2">
                    <div className="flex items-center gap-1.5 text-[11px]">
                      <Loader2 className="h-3.5 w-3.5 animate-spin text-violet-300" />
                      <span className="truncate">
                        {prep?.day}
                        {prep && prep.day_total > 1 && ` (${prep.day_index + 1}/${prep.day_total})`}
                        {" · "}
                        {STAGES[prep?.stage ?? ""] ?? "Préparation"}
                      </span>
                    </div>
                    {prep?.message && (
                      <div className="truncate text-[10px] text-muted-foreground">{prep.message}</div>
                    )}
                    <div className="h-1 overflow-hidden rounded bg-muted">
                      <div
                        className="h-full bg-violet-400 transition-all"
                        style={{ width: `${Math.round((prep?.progress ?? 0) * 100)}%` }}
                      />
                    </div>
                  </div>
                )}
                {!running && prep?.state === "done" && prep.skipped.length > 0 && (
                  <p className="text-[10px] text-amber-300/90">
                    Jours ignorés : {prep.skipped.map(([dd, e]) => `${dd} (${e})`).join(" · ")}
                  </p>
                )}
                {!running && prep?.state === "error" && prep.error && (
                  <p className="text-[10px] text-red-400">{prep.error}</p>
                )}

                <div className="flex flex-wrap items-center gap-1.5 pt-1">
                  {d?.launchable && (
                    <button
                      onClick={() => launch(firstStart)}
                      className="flex items-center gap-1.5 rounded bg-emerald-600/80 px-3 py-1 text-[11px] font-medium text-white hover:bg-emerald-600"
                    >
                      <Play className="h-3.5 w-3.5" /> Lancer · {firstStart} ET
                    </button>
                  )}
                  {d && (d.state === "not_prepared" || d.state === "incomplete") && !dayPending && (
                    <>
                      <button
                        disabled={chosen.length === 0}
                        onClick={() => prepare([day], false, true)}
                        title={
                          running
                            ? "Ce jour passe devant le téléchargement en cours, qui reprend ensuite"
                            : undefined
                        }
                        className={cn(
                          "flex items-center gap-1.5 rounded px-3 py-1 text-[11px] font-medium disabled:opacity-40",
                          d.launchable
                            ? "border border-border text-muted-foreground hover:bg-accent hover:text-foreground"
                            : "bg-violet-600/80 text-white hover:bg-violet-600",
                        )}
                      >
                        <Download className="h-3.5 w-3.5" />
                        {d.state === "incomplete" ? "Compléter" : "Préparer"}
                      </button>
                      {!scannedAll && !d.launchable && (
                        <button
                          disabled={chosen.length === 0}
                          onClick={() => prepare([day], true, true)}
                          title="Minutes, pré-scan et confirmation seulement — pour voir les opportunités avant de télécharger les ticks"
                          className="rounded border border-border px-2 py-1 text-[11px] text-muted-foreground hover:bg-accent hover:text-foreground disabled:opacity-40"
                        >
                          Scanner seulement
                        </button>
                      )}
                    </>
                  )}
                  {running && (
                    <button
                      onClick={() => api.replayDataCancel().catch(() => {})}
                      title="Arrête le téléchargement et vide la file ; ce qui est déjà sur disque reste utilisable"
                      className="flex items-center gap-1.5 rounded border border-border px-2 py-1 text-[11px] text-muted-foreground hover:bg-accent hover:text-foreground"
                    >
                      <Square className="h-3.5 w-3.5" /> Arrêter
                    </button>
                  )}
                  <span className="flex-1" />
                  <button
                    onClick={() => api.openReplayDataFolder().catch(() => {})}
                    title="Ouvrir le dossier des données de replay"
                    className="rounded p-1 text-muted-foreground hover:bg-accent hover:text-foreground"
                  >
                    <FolderOpen className="h-3.5 w-3.5" />
                  </button>
                </div>
              </>
            )}
            {err && <p className="text-[10px] text-red-400">{err}</p>}
          </div>
        </div>
      </DialogContent>
    </Dialog>
  );
}
