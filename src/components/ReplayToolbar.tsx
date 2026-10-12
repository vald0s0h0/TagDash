import { useEffect, useRef, useState } from "react";
import {
  AlarmClock,
  ArrowRight,
  Bell,
  CalendarDays,
  ChevronsRight,
  Download,
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
import { bindingLabel, setReplayActive, useHotkeyStore } from "@/stores/hotkeyStore";
import { DayPickerModal } from "@/components/DayPickerModal";
import type { ReplayPrepareStatus, ReplayStatus } from "@/types";

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
  trades: {
    label: "TRADES",
    title:
      "Vrais prints et quotes SIP autour des opportunités préparées, tranches 10 s synthétisées des barres 1 min ailleurs",
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
    title: "Barres 1 min synthétisées — jour préparé sans ticks (scan seulement)",
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
 * WHERE the replayed day came from — `ReplayStatus.provider`. A replay reads only
 * the day prepared on disk (`replay_data`), downloaded from Alpaca SIP.
 */
const PROVIDERS: Record<string, Badge> = {
  alpaca_sip: {
    label: "SIP",
    title: "Alpaca SIP historique, préparé sur disque — le replay ne lit que ce qui y est",
    className: "bg-violet-500/20 text-violet-300",
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
  // The strategies the day was prepared for — the replay runs them whatever their
  // live toggle. Empty until a day is picked: every preparable strategy.
  const [strategies, setStrategies] = useState<string[]>([]);
  const [prep, setPrep] = useState<ReplayPrepareStatus | null>(null);
  const playPauseKey = useHotkeyStore((s) => s.bindings.replay_play_pause);
  const nextAlertKey = useHotkeyStore((s) => s.bindings.replay_next_alert);
  const [startErr, setStartErr] = useState<string | null>(null);
  const [localPlaying, setLocalPlaying] = useState<boolean | null>(null);
  const [calendarOpen, setCalendarOpen] = useState(false);
  const pollRef = useRef<number | null>(null);
  const busyRef = useRef(false);

  // Fast while a session is open (the clock must visibly run the moment Play is
  // pressed), slow when idle. The command is a cheap read.
  useEffect(() => {
    if (!replayOpen) return;
    const tick = () => api.getReplayStatus().then(setStatus).catch(() => {});
    tick();
    const interval = status?.active ? 400 : 2_000;
    pollRef.current = window.setInterval(tick, interval);
    return () => {
      if (pollRef.current !== null) window.clearInterval(pollRef.current);
    };
  }, [replayOpen, status?.active]);

  // The replay shortcuts (play / pause, next alert) only take their key while a
  // replay runs.
  useEffect(() => {
    setReplayActive(status?.active ?? false);
  }, [status?.active]);

  // A preparation running in the background (a month queued, or the rest of the day
  // being replayed): shown here because the modal is closed most of that time.
  useEffect(() => {
    if (!replayOpen) return;
    const tick = () => api.replayDataStatus().then(setPrep).catch(() => {});
    tick();
    const id = window.setInterval(tick, prep?.running ? 1_000 : 3_000);
    return () => window.clearInterval(id);
  }, [replayOpen, prep?.running]);

  // The optimistic Play/Pause state holds until the engine agrees — or 1.5 s at
  // most, so a command the engine could not honour never leaves the button lying.
  useEffect(() => {
    if (localPlaying === null) return;
    if (status && status.playing === localPlaying) {
      setLocalPlaying(null);
      return;
    }
    const id = window.setTimeout(() => setLocalPlaying(null), 1_500);
    return () => window.clearTimeout(id);
  }, [status, localPlaying]);

  if (!replayOpen) return null;

  const active = status?.active ?? false;
  const st = status?.state ?? "idle";
  const playing = localPlaying ?? status?.playing ?? false;
  const loading = st === "loading";
  const seeking = st === "seeking";
  const buffering = playing && (status?.buffering ?? false);
  const shortcut = (b: typeof playPauseKey) => (b ? ` (${bindingLabel(b)})` : "");

  const start = async (d = day, hmStart = startHm, ids = strategies) => {
    if (!d) return;
    setStartErr(null);
    try {
      await api.replayStart(d, hmStart, ids);
    } catch (e) {
      setStartErr(String(e));
    }
  };

  const prepChip = prep?.running && (
    <span
      title={[
        "Téléchargement des données de replay en arrière-plan",
        prep.message,
        prep.queue.length > 0 ? `${prep.queue.length} jour${prep.queue.length > 1 ? "s" : ""} en attente` : null,
      ].filter(Boolean).join(" — ")}
      className="flex items-center gap-1 rounded border border-violet-500/30 bg-violet-500/10 px-1.5 py-0.5 text-[10px] tabular-nums text-violet-200"
    >
      <Download className="h-3 w-3 animate-pulse" />
      {prep.day}
      {prep.day_total > 1 && ` · ${Math.min(prep.day_index + 1, prep.day_total)}/${prep.day_total}`}
    </span>
  );

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
    <div data-usage-area="replay-toolbar" className="flex items-center gap-2 border-b border-amber-600/40 bg-amber-950/30 px-3 py-1.5 text-xs">
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
            title="Préparer une séance"
            onClick={() => setCalendarOpen(true)}
            className={cn(
              "flex h-7 items-center gap-1.5 rounded border px-2 text-xs transition-colors",
              day
                ? "border-border bg-background text-foreground"
                : "border-amber-500/40 bg-amber-500/10 text-amber-300 animate-pulse",
            )}
          >
            <CalendarDays className="h-3.5 w-3.5" />
            {day ?? "Préparer une séance"}
          </button>
          {day && (
            <>
              <select
                value={startHm}
                onChange={(e) => setStartHm(e.target.value)}
                title="Heure de départ (ET)"
                className="h-7 rounded border border-border bg-background px-1.5 text-xs text-foreground"
              >
                {[...new Set([...SESSION_STARTS, startHm])].sort().map((s) => (
                  <option key={s} value={s}>{s} ET</option>
                ))}
              </select>
              <Btn title="Démarrer le replay" onClick={() => start()}>
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
          {status?.ready_until && (
            <span
              title="Jour encore en préparation : ses opportunités sont connues et téléchargées jusqu'à cette heure, la suite arrive pendant le replay. La lecture s'y arrête et attend si elle la rattrape."
              className="rounded bg-violet-500/15 px-1.5 py-0.5 text-[10px] tabular-nums text-violet-200"
            >
              prêt → {nyTime(status.ready_until, false)}
            </span>
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

          {/* Transport. Play/Pause is never swallowed by the busy guard: it is
              the one control that must always answer. */}
          <button
            title={(playing ? "Pause" : "Lecture") + shortcut(playPauseKey)}
            onClick={() => {
              const next = !playing;
              setLocalPlaying(next);
              api.replaySetPlaying(next).catch(() => setLocalPlaying(null));
            }}
            className={cn(
              "flex h-8 w-8 items-center justify-center rounded-full transition-colors",
              playing
                ? "bg-emerald-500/25 text-emerald-300 hover:bg-emerald-500/35"
                : "bg-amber-500/20 text-amber-300 hover:bg-amber-500/30",
            )}
          >
            {playing ? <Pause className="h-4 w-4" /> : <Play className="h-4 w-4" />}
          </button>
          <span
            className={cn(
              "flex min-w-[88px] items-center justify-center gap-1 rounded px-1.5 py-0.5 text-[10px] font-semibold uppercase tabular-nums",
              seeking
                ? "bg-sky-500/20 text-sky-300"
                : buffering
                  ? "bg-violet-500/20 text-violet-200"
                  : playing
                    ? "bg-emerald-500/20 text-emerald-300"
                    : "bg-muted text-muted-foreground",
            )}
            title={
              buffering
                ? "La lecture a rattrapé le téléchargement du jour : elle reprend dès que la suite est prête"
                : "État du replay"
            }
          >
            {seeking ? (
              <>
                <Loader2 className="h-3 w-3 animate-spin" /> Rattrapage
              </>
            ) : buffering ? (
              <>
                <Loader2 className="h-3 w-3 animate-spin" /> Données…
              </>
            ) : playing ? (
              <>
                <span className="h-1.5 w-1.5 animate-pulse rounded-full bg-emerald-400" />
                Lecture ×{status?.speed ?? 1}
              </>
            ) : st === "ended" ? (
              "Fin"
            ) : (
              "Pause"
            )}
          </span>
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
            title={
              "Passer directement à la prochaine opportunité préparée : le ticker s'affiche et la lecture continue" +
              shortcut(nextAlertKey)
            }
            onClick={() => cmd(() => api.replayNextAlert())}
            activeCls={status?.next_alert_armed}
          >
            <Bell className="h-3.5 w-3.5" /> Prochaine alerte
          </Btn>
          {status?.notice && (
            <span className="truncate text-[10px] text-muted-foreground" title={status.notice}>
              {status.notice}
            </span>
          )}
          <Btn
            title="Passer à la séance suivante (même heure de départ)"
            onClick={() => cmd(() => api.replayNextDay())}
          >
            <ChevronsRight className="h-3.5 w-3.5" /> Jour suivant
          </Btn>

          <span className="flex-1" />

          {prepChip}
          <Btn title="Arrêter le replay et revenir au temps réel" onClick={stop}>
            <Square className="h-3.5 w-3.5" /> Quitter
          </Btn>
        </>
      )}

      {!active && <span className="flex-1" />}
      {!active && !loading && prepChip}
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
        onLaunch={(d, hmStart, ids) => {
          setDay(d);
          setStartHm(hmStart);
          setStrategies(ids);
          start(d, hmStart, ids);
        }}
      />
    </div>
  );
}
