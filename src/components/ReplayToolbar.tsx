// Market Replay toolbar. Rendered only when activated from the LeftRail menu
// (uiStore.replayOpen). Drives the backend replay engine through the replay_*
// commands and polls get_replay_status (~500 ms) for the simulated clock, the
// loading progress and the transport state.
//
// While a replay is active the whole platform runs on the simulated day: live
// feeds are stopped, the scanner / strategy engines / internal trading / journal
// all follow the simulated clock; closing the replay restores live mode.

import { useEffect, useRef, useState } from "react";
import {
  AlarmClock,
  Bell,
  CalendarDays,
  ChevronsRight,
  FastForward,
  History,
  Loader2,
  Pause,
  Play,
  Rewind,
  Square,
  X,
} from "lucide-react";
import { api } from "@/lib/tauri";
import { cn } from "@/lib/utils";
import { nyTime } from "@/lib/nyTime";
import { useUiStore } from "@/stores/uiStore";
import type { ReplayStatus } from "@/types";

const SPEEDS = [0.5, 1, 2, 5, 10, 30, 60, 120];
const SESSION_STARTS = ["04:00", "07:00", "09:30"] as const;

/** Most recent weekday strictly before today (default replay date). */
function lastWeekday(): string {
  const d = new Date();
  do {
    d.setDate(d.getDate() - 1);
  } while (d.getDay() === 0 || d.getDay() === 6);
  return d.toISOString().slice(0, 10);
}

export function ReplayToolbar() {
  const replayOpen = useUiStore((s) => s.replayOpen);
  const toggleReplay = useUiStore((s) => s.toggleReplay);

  const [status, setStatus] = useState<ReplayStatus | null>(null);
  const [day, setDay] = useState<string>(lastWeekday());
  const [startHm, setStartHm] = useState<string>("04:00");
  const [startErr, setStartErr] = useState<string | null>(null);
  const pollRef = useRef<number | null>(null);

  // Poll the backend status while the toolbar is shown.
  useEffect(() => {
    if (!replayOpen) return;
    const tick = () => api.getReplayStatus().then(setStatus).catch(() => {});
    tick();
    pollRef.current = window.setInterval(tick, 500);
    return () => {
      if (pollRef.current !== null) window.clearInterval(pollRef.current);
    };
  }, [replayOpen]);

  if (!replayOpen) return null;

  const active = status?.active ?? false;
  const st = status?.state ?? "idle";
  const playing = status?.playing ?? false;
  const loading = st === "loading";

  const start = async () => {
    setStartErr(null);
    try {
      await api.replayStart(day, startHm);
    } catch (e) {
      setStartErr(String(e));
    }
  };

  const stop = () => api.replayStop().catch(() => {});

  const Btn = ({
    title,
    onClick,
    disabled,
    children,
    activeCls,
  }: {
    title: string;
    onClick: () => void;
    disabled?: boolean;
    children: React.ReactNode;
    activeCls?: boolean;
  }) => (
    <button
      title={title}
      onClick={onClick}
      disabled={disabled}
      className={cn(
        "flex h-7 items-center gap-1 rounded px-2 text-xs text-muted-foreground transition-colors",
        "hover:bg-accent hover:text-foreground disabled:opacity-40 disabled:pointer-events-none",
        activeCls && "bg-amber-500/20 text-amber-300"
      )}
    >
      {children}
    </button>
  );

  return (
    <div className="flex items-center gap-2 border-b border-amber-600/40 bg-amber-950/30 px-3 py-1.5 text-xs">
      <span className="flex items-center gap-1.5 font-semibold uppercase tracking-wider text-amber-400">
        <History className="h-4 w-4" />
        Replay
      </span>

      {!active && (
        <>
          <CalendarDays className="h-4 w-4 text-muted-foreground" />
          <input
            type="date"
            value={day}
            onChange={(e) => setDay(e.target.value)}
            className="h-7 rounded border border-border bg-background px-2 text-xs text-foreground"
          />
          <select
            value={startHm}
            onChange={(e) => setStartHm(e.target.value)}
            title="Heure de départ (ET)"
            className="h-7 rounded border border-border bg-background px-1.5 text-xs text-foreground"
          >
            {SESSION_STARTS.map((s) => (
              <option key={s} value={s}>{s} ET</option>
            ))}
          </select>
          <Btn title="Démarrer le replay" onClick={start}>
            <Play className="h-3.5 w-3.5" /> Démarrer
          </Btn>
          {startErr && <span className="text-red-400">{startErr}</span>}
          {st === "error" && status?.error && (
            <span className="truncate text-red-400" title={status.error}>{status.error}</span>
          )}
        </>
      )}

      {loading && (
        <>
          <span className="flex items-center gap-2 text-amber-300">
            <Loader2 className="h-3.5 w-3.5 animate-spin" />
            Chargement {status?.day} · {Math.round((status?.progress ?? 0) * 100)} %
          </span>
          <span className="flex-1" />
          <Btn title="Annuler et revenir au temps réel" onClick={stop}>
            <Square className="h-3.5 w-3.5" /> Annuler
          </Btn>
        </>
      )}

      {active && !loading && (
        <>
          {/* Day + source + simulated NY clock */}
          <span className="font-medium text-foreground">{status?.day}</span>
          <span
            className={cn(
              "rounded px-1.5 py-0.5 text-[10px] font-semibold uppercase",
              status?.source === "tape"
                ? "bg-emerald-500/20 text-emerald-300"
                : "bg-sky-500/20 text-sky-300"
            )}
            title={
              status?.source === "tape"
                ? "Trades réels enregistrés (granularité fine)"
                : "Barres 1 min synthétisées (pas de tape pour ce jour)"
            }
          >
            {status?.source === "tape" ? "TAPE" : "MINUTES"}
          </span>
          <span className="min-w-[72px] font-mono text-base font-semibold tabular-nums text-amber-300">
            {status?.sim_time ? nyTime(status.sim_time, true) : "—"}
          </span>
          {st === "ended" && (
            <span className="text-[10px] uppercase text-muted-foreground">fin de séance</span>
          )}

          <span className="mx-1 h-4 w-px bg-border" />

          {/* Session jump points */}
          {SESSION_STARTS.map((hm) => (
            <Btn
              key={hm}
              title={`Aller à ${hm} ET (retour en arrière = rejoue depuis le début)`}
              onClick={() => api.replaySeekClock(hm).catch(() => {})}
            >
              <AlarmClock className="h-3 w-3" />
              {hm}
            </Btn>
          ))}

          <span className="mx-1 h-4 w-px bg-border" />

          {/* Transport */}
          <Btn title="Reculer de 10 min" onClick={() => api.replaySeekRelative(-600).catch(() => {})}>
            <Rewind className="h-3.5 w-3.5" /> 10m
          </Btn>
          <Btn title="Reculer de 1 min" onClick={() => api.replaySeekRelative(-60).catch(() => {})}>
            <Rewind className="h-3 w-3" /> 1m
          </Btn>
          <button
            title={playing ? "Pause" : "Lecture"}
            onClick={() => api.replaySetPlaying(!playing).catch(() => {})}
            className="flex h-8 w-8 items-center justify-center rounded-full bg-amber-500/20 text-amber-300 hover:bg-amber-500/30"
          >
            {playing ? <Pause className="h-4 w-4" /> : <Play className="h-4 w-4" />}
          </button>
          <Btn title="Avancer de 1 min" onClick={() => api.replaySeekRelative(60).catch(() => {})}>
            1m <FastForward className="h-3 w-3" />
          </Btn>
          <Btn title="Avancer de 10 min" onClick={() => api.replaySeekRelative(600).catch(() => {})}>
            10m <FastForward className="h-3.5 w-3.5" />
          </Btn>

          {/* Speed */}
          <select
            value={String(status?.speed ?? 1)}
            onChange={(e) => api.replaySetSpeed(Number(e.target.value)).catch(() => {})}
            title="Vitesse de lecture"
            className="h-7 rounded border border-border bg-background px-1.5 text-xs text-foreground"
          >
            {SPEEDS.map((s) => (
              <option key={s} value={String(s)}>×{s}</option>
            ))}
          </select>

          <span className="mx-1 h-4 w-px bg-border" />

          <Btn
            title="Avancer en accéléré jusqu'à la prochaine alerte scanner, puis pause"
            onClick={() => api.replayNextAlert().catch(() => {})}
            activeCls={status?.next_alert_armed}
          >
            <Bell className="h-3.5 w-3.5" /> Prochaine alerte
          </Btn>
          <Btn
            title="Passer à la séance suivante (même heure de départ)"
            onClick={() => api.replayNextDay().catch(() => {})}
          >
            <ChevronsRight className="h-3.5 w-3.5" /> Jour suivant
          </Btn>

          <span className="flex-1" />

          <Btn title="Arrêter le replay et revenir au temps réel" onClick={stop}>
            <Square className="h-3.5 w-3.5" /> Quitter
          </Btn>
        </>
      )}

      {!active && <span className="flex-1" />}
      {!active && (
        <button
          title="Fermer la barre de replay"
          onClick={toggleReplay}
          className="flex h-7 w-7 items-center justify-center rounded text-muted-foreground hover:bg-accent hover:text-foreground"
        >
          <X className="h-4 w-4" />
        </button>
      )}
    </div>
  );
}
