import { useEffect, useRef, useState } from "react";
import {
  AlarmClock,
  ArrowRight,
  Bell,
  CalendarDays,
  ChevronsRight,
  FastForward,
  History,
  Loader2,
  Pause,
  Play,
  SkipForward,
  Square,
  X,
} from "lucide-react";
import { api } from "@/lib/tauri";
import { cn } from "@/lib/utils";
import { nyTime } from "@/lib/nyTime";
import { useUiStore } from "@/stores/uiStore";
import { DayPickerModal } from "@/components/DayPickerModal";
import type { ReplayStatus } from "@/types";

const SPEEDS = [0.5, 1, 2, 5, 10, 30, 60, 120];
const SESSION_STARTS = ["04:00", "07:00", "09:30"] as const;

type Badge = { label: string; title: string; className: string };

/** Granularity of the replayed price path — `ReplayStatus.source`. */
const GRANULARITY: Record<string, Badge> = {
  tape: {
    label: "TAPE",
    title: "Trades réels enregistrés (granularité fine)",
    className: "bg-emerald-500/20 text-emerald-300",
  },
  seconds: {
    label: "1 SEC",
    title:
      "Agrégats 1 seconde réels sur les symboles affichés — rien n'est synthétisé sur ces titres",
    className: "bg-emerald-500/20 text-emerald-300",
  },
  minutes: {
    label: "MINUTES",
    title: "Barres 1 min synthétisées (pas de tape pour ce jour)",
    className: "bg-sky-500/20 text-sky-300",
  },
  daily: {
    label: "DAILY",
    title:
      "Aucune donnée intraday disponible — barre daily unique simulée sur la séance",
    className: "bg-amber-500/20 text-amber-300",
  },
};

/**
 * WHERE the replayed day came from — `ReplayStatus.provider`. Same priority as the
 * live feed (SIP > Massive Starter+ > flat files), so the badge answers the one
 * question a surprising replay raises: which tape am I actually watching?
 */
const PROVIDERS: Record<string, Badge> = {
  alpaca_sip: {
    label: "SIP",
    title: "Alpaca SIP — tape consolidé complet (source prioritaire)",
    className: "bg-violet-500/20 text-violet-300",
  },
  massive: {
    label: "MASSIVE",
    title:
      "Massive Starter+ — agrégats seconde/minute sur tout le marché US. Le délai de 15 min du live n'existe pas ici : la séance est de l'historique.",
    className: "bg-violet-500/20 text-violet-300",
  },
  alpaca_iex: {
    label: "IEX",
    title:
      "Alpaca plan gratuit — uniquement les prints IEX (quelques % du volume). Un flat file téléchargé serait plus complet.",
    className: "bg-amber-500/20 text-amber-300",
  },
  flat_files: {
    label: "FLAT FILES",
    title: "Fichiers plats locaux — aucune requête réseau",
    className: "bg-slate-500/20 text-slate-300",
  },
  daily_cache: {
    label: "CACHE DAILY",
    title:
      "Dernier recours : aucune source intraday n'a répondu, la séance est reconstruite depuis la seule barre daily locale",
    className: "bg-amber-500/20 text-amber-300",
  },
};

function EditableTime({
  simTime,
  onSeek,
}: {
  simTime: string | null;
  onSeek: (hm: string) => void;
}) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState("");
  const inputRef = useRef<HTMLInputElement>(null);
  const display = simTime ? nyTime(simTime, true) : "—";

  const startEditing = () => {
    setDraft(simTime ? nyTime(simTime, false) : "04:00");
    setEditing(true);
  };

  useEffect(() => {
    if (editing) inputRef.current?.focus();
  }, [editing]);

  const commit = () => {
    const cleaned = draft.trim();
    if (/^\d{1,2}:\d{2}$/.test(cleaned)) {
      onSeek(cleaned);
    }
    setEditing(false);
  };

  if (editing) {
    return (
      <span className="flex items-center gap-0.5">
        <input
          ref={inputRef}
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") commit();
            if (e.key === "Escape") setEditing(false);
          }}
          onBlur={() => setEditing(false)}
          className="w-[52px] rounded border border-amber-500/40 bg-background px-1 py-0.5 font-mono text-sm font-semibold tabular-nums text-amber-300 outline-none focus:border-amber-400"
          placeholder="HH:MM"
        />
        <button
          onMouseDown={(e) => e.preventDefault()}
          onClick={commit}
          className="flex h-5 w-5 items-center justify-center rounded text-amber-300/60 hover:bg-amber-500/20 hover:text-amber-300"
          title="Aller à cette heure"
        >
          <ArrowRight className="h-3.5 w-3.5" />
        </button>
      </span>
    );
  }

  return (
    <span
      onClick={startEditing}
      className="min-w-[72px] cursor-pointer font-mono text-base font-semibold tabular-nums text-amber-300 hover:text-amber-200"
      title="Cliquer pour modifier l'heure"
    >
      {display}
    </span>
  );
}

export function ReplayToolbar() {
  const replayOpen = useUiStore((s) => s.replayOpen);
  const toggleReplay = useUiStore((s) => s.toggleReplay);

  const [status, setStatus] = useState<ReplayStatus | null>(null);
  const [day, setDay] = useState<string | null>(null);
  const [startHm, setStartHm] = useState<string>("04:00");
  const [startErr, setStartErr] = useState<string | null>(null);
  const [localPlaying, setLocalPlaying] = useState<boolean | null>(null);
  const [calendarOpen, setCalendarOpen] = useState(false);
  const pollRef = useRef<number | null>(null);
  const busyRef = useRef(false);

  useEffect(() => {
    if (!replayOpen) return;
    const tick = () => api.getReplayStatus().then(setStatus).catch(() => {});
    tick();
    const interval = status?.playing ? 500 : 2_000;
    pollRef.current = window.setInterval(tick, interval);
    return () => {
      if (pollRef.current !== null) window.clearInterval(pollRef.current);
    };
  }, [replayOpen, status?.playing]);

  useEffect(() => {
    if (status && localPlaying !== null && status.playing === localPlaying) {
      setLocalPlaying(null);
    }
  }, [status, localPlaying]);

  if (!replayOpen) return null;

  const active = status?.active ?? false;
  const st = status?.state ?? "idle";
  const playing = localPlaying ?? status?.playing ?? false;
  const loading = st === "loading";

  const start = async () => {
    if (!day) return;
    setStartErr(null);
    try {
      await api.replayStart(day, startHm);
    } catch (e) {
      setStartErr(String(e));
    }
  };

  const stop = () => api.replayStop().catch(() => {});

  const cmd = (fn: () => Promise<void>) => {
    if (busyRef.current) return;
    busyRef.current = true;
    fn().catch(() => {}).finally(() => { busyRef.current = false; });
  };

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

      {/* A replay is always simulated, whatever broker is selected. Said here rather
          than left implicit: with Alpaca armed, the natural assumption is that the
          buy button still reaches the venue — it does not, and it must not. */}
      {active && (
        <span
          title="Un replay est toujours simulé : aucun ordre n'est envoyé au broker, quel que soit le broker sélectionné."
          className="rounded border border-amber-500/30 bg-amber-500/10 px-1.5 py-0.5 text-[10px] font-medium text-amber-300/90"
        >
          Ordres simulés
        </span>
      )}

      {!active && (
        <>
          <button
            title="Choisir un jour"
            onClick={() => setCalendarOpen(true)}
            className={cn(
              "flex h-7 items-center gap-1.5 rounded border px-2 text-xs transition-colors",
              day
                ? "border-border bg-background text-foreground"
                : "border-amber-500/40 bg-amber-500/10 text-amber-300 animate-pulse",
            )}
          >
            <CalendarDays className="h-3.5 w-3.5" />
            {day ?? "Sélectionner un jour"}
          </button>
          {day && (
            <>
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
            </>
          )}
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
          <span className="font-medium text-foreground">{status?.day}</span>
          <span
            className={cn(
              "rounded px-1.5 py-0.5 text-[10px] font-semibold uppercase",
              GRANULARITY[status?.source ?? ""]?.className ??
                "bg-sky-500/20 text-sky-300"
            )}
            title={
              GRANULARITY[status?.source ?? ""]?.title ??
              "Barres 1 min synthétisées (pas de tape pour ce jour)"
            }
          >
            {GRANULARITY[status?.source ?? ""]?.label ?? "MINUTES"}
          </span>
          {status?.provider && (
            <span
              className={cn(
                "rounded px-1.5 py-0.5 text-[10px] font-semibold uppercase",
                PROVIDERS[status.provider]?.className ??
                  "bg-muted text-muted-foreground"
              )}
              title={
                PROVIDERS[status.provider]?.title ??
                "Source de données du replay"
              }
            >
              {PROVIDERS[status.provider]?.label ?? status.provider}
            </span>
          )}
          <EditableTime
            simTime={status?.sim_time ?? null}
            onSeek={(hm) => cmd(() => api.replaySeekClock(hm))}
          />
          {st === "ended" && (
            <span className="text-[10px] uppercase text-muted-foreground">fin de séance</span>
          )}

          <span className="mx-1 h-4 w-px bg-border" />

          {SESSION_STARTS.map((hm) => (
            <Btn
              key={hm}
              title={`Aller à ${hm} ET`}
              onClick={() => cmd(() => api.replaySeekClock(hm))}
            >
              <AlarmClock className="h-3 w-3" />
              {hm}
            </Btn>
          ))}

          <span className="mx-1 h-4 w-px bg-border" />

          {/* Transport */}
          <button
            title={playing ? "Pause" : "Lecture"}
            onClick={() => {
              const next = !playing;
              setLocalPlaying(next);
              cmd(() => api.replaySetPlaying(next));
            }}
            className="flex h-8 w-8 items-center justify-center rounded-full bg-amber-500/20 text-amber-300 hover:bg-amber-500/30"
          >
            {playing ? <Pause className="h-4 w-4" /> : <Play className="h-4 w-4" />}
          </button>
          <Btn
            title="Avancer de 1 min"
            onClick={() => cmd(() => api.replaySeekRelative(60))}
          >
            1m <FastForward className="h-3 w-3" />
          </Btn>
          <Btn
            title="Avancer de 10 min"
            onClick={() => cmd(() => api.replaySeekRelative(600))}
          >
            10m <FastForward className="h-3.5 w-3.5" />
          </Btn>
          <Btn
            title="Barre suivante (prochain close 1 min)"
            onClick={() => cmd(() => api.replayNextBar())}
          >
            <SkipForward className="h-3.5 w-3.5" /> Barre
          </Btn>

          {/* Speed */}
          <select
            value={String(status?.speed ?? 1)}
            onChange={(e) => cmd(() => api.replaySetSpeed(Number(e.target.value)))}
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
            onClick={() => cmd(() => api.replayNextAlert())}
            activeCls={status?.next_alert_armed}
          >
            <Bell className="h-3.5 w-3.5" /> Prochaine alerte
          </Btn>
          <Btn
            title="Passer à la séance suivante (même heure de départ)"
            onClick={() => cmd(() => api.replayNextDay())}
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

      <DayPickerModal
        open={calendarOpen}
        onClose={() => setCalendarOpen(false)}
        onSelect={(d) => setDay(d)}
      />
    </div>
  );
}
