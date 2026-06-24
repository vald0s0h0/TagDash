// Dictée vocale — microphone check + STT job queue. Opened from the bottom-left ⋮
// menu. Shows the whisper model status (with a model picker + download button for
// more precision), a mic tester with a live level meter + device picker, the trading
// jargon list that biases recognition, and the single-worker job queue with per-job
// cancel/retry and the current pause reason.

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { listen } from "@tauri-apps/api/event";
import { Download, Loader2, Mic, RotateCw, Square, X } from "lucide-react";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { api } from "@/lib/tauri";
import { cn } from "@/lib/utils";
import { useSttStore } from "@/stores/sttStore";
import { useLocalConfig, useUpdateLocalConfig } from "@/queries/useLocalConfig";
import type { AppConfig, SttJobState, SttSpectrum } from "@/types";

const MIC_BARS = 32;
const TEST_OWNER = "stt-mic-test";

interface Props {
  open: boolean;
  onClose: () => void;
}

const STATE_BADGE: Record<SttJobState, string> = {
  queued: "bg-zinc-700/70 text-zinc-300",
  running: "bg-blue-900/60 text-blue-300 animate-pulse",
  done: "bg-emerald-900/60 text-emerald-300",
  error: "bg-red-900/70 text-red-300",
  cancelled: "bg-zinc-800/70 text-zinc-500",
};

const STATE_LABEL: Record<SttJobState, string> = {
  queued: "En file",
  running: "En cours",
  done: "Terminé",
  error: "Erreur",
  cancelled: "Annulé",
};

export function SttModal({ open, onClose }: Props) {
  const status = useSttStore((s) => s.status);
  const refresh = useSttStore((s) => s.refresh);
  const { data: config } = useLocalConfig();
  const updateConfig = useUpdateLocalConfig();

  const [jargonText, setJargonText] = useState("");

  // Live mic monitor — reuses the exact same capture + spectrum path as dictation
  // (so the audiowave actually moves), but discards the audio on stop (no job).
  const [monitoring, setMonitoring] = useState(false);
  const [bins, setBins] = useState<number[]>(() => new Array(MIC_BARS).fill(0));
  const monitorRef = useRef<{ unlisten?: () => void }>({});
  const monitoringRef = useRef(false);
  useEffect(() => { monitoringRef.current = monitoring; }, [monitoring]);
  const owner = useSttStore((s) => s.owner);
  const micBusy = owner !== null && owner !== TEST_OWNER;

  const { data: devices } = useQuery({
    queryKey: ["stt-input-devices"],
    queryFn: api.sttListInputDevices,
    enabled: open,
  });

  // Poll while open (events also refresh, this covers download progress smoothly).
  useEffect(() => {
    if (!open) return;
    refresh();
    const id = setInterval(refresh, 1200);
    return () => clearInterval(id);
  }, [open, refresh]);

  // Seed the jargon editor from config (on open / when it changes upstream).
  useEffect(() => {
    if (config) setJargonText((config.stt?.jargon ?? []).join("\n"));
  }, [config?.stt?.jargon, open]);

  const jobs = useMemo(
    () => (status?.jobs ?? []).slice().reverse(), // newest first
    [status?.jobs],
  );

  const patchStt = (patch: Partial<AppConfig["stt"]>) => {
    if (!config) return;
    updateConfig.mutate({ ...config, stt: { ...config.stt, ...patch } });
  };

  const stopTest = useCallback(() => {
    monitorRef.current.unlisten?.();
    monitorRef.current = {};
    if (monitoringRef.current) api.sttCancelRecording().catch(() => {});
    useSttStore.getState().release(TEST_OWNER);
    setMonitoring(false);
    refresh();
  }, [refresh]);

  const toggleTest = async () => {
    if (monitoringRef.current) { stopTest(); return; }
    if (!useSttStore.getState().claim(TEST_OWNER)) return;
    try {
      await api.sttStartRecording("diary", null, null);
    } catch {
      useSttStore.getState().release(TEST_OWNER);
      return;
    }
    setMonitoring(true);
    setBins(new Array(MIC_BARS).fill(0));
    const un = await listen<SttSpectrum>("stt-spectrum", (e) => setBins(e.payload.bins));
    monitorRef.current = { unlisten: un };
    refresh();
  };

  // Stop monitoring when the modal closes / unmounts (never leave the mic hot).
  useEffect(() => {
    if (!open && monitoringRef.current) stopTest();
  }, [open, stopTest]);
  useEffect(() => () => {
    monitorRef.current.unlisten?.();
    if (monitoringRef.current) api.sttCancelRecording().catch(() => {});
  }, []);

  const saveJargon = () => {
    const arr = jargonText.split("\n").map((s) => s.trim()).filter(Boolean);
    patchStt({ jargon: arr });
  };

  const download = () => api.sttDownloadModel().catch(() => {});

  return (
    <Dialog open={open} onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="max-w-lg overflow-hidden">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <Mic className="h-4 w-4 text-muted-foreground" />
            Dictée vocale — micro &amp; file
          </DialogTitle>
        </DialogHeader>

        {/* Single min-w-0 wrapper: the DialogContent is a CSS grid, whose items
            default to min-width:auto — long transcription text would otherwise
            stretch the whole dialog. */}
        <div className="min-w-0 space-y-3">
          {/* Worker / pause banner */}
          <div className="flex items-center justify-between gap-2 rounded-md border border-border bg-background/50 px-3 py-2 text-xs">
            <span className="truncate text-muted-foreground">
              Worker :{" "}
              <span className="font-medium text-foreground">{status?.worker_state ?? "—"}</span>
            </span>
            {status?.paused_reason && (
              <span className="shrink-0 rounded bg-amber-900/50 px-1.5 py-0.5 text-[11px] text-amber-300">
                en pause · {status.paused_reason}
              </span>
            )}
          </div>

          {/* Model — picker (small/medium = precision) + download */}
          <div className="space-y-1">
            <label className="text-[11px] uppercase tracking-wide text-muted-foreground">
              Modèle
            </label>
            <div className="flex items-center gap-2">
              <select
                value={config?.stt?.model ?? "small"}
                onChange={(e) => patchStt({ model: e.target.value as "small" | "medium" })}
                className="min-w-0 rounded border border-border bg-background px-2 py-1.5 text-xs text-foreground outline-none"
              >
                <option value="small">small (~466 Mo · rapide)</option>
                <option value="medium">medium (~1,5 Go · + précis)</option>
              </select>
              {status?.model_present ? (
                <span className="shrink-0 text-sm text-emerald-400">Installé ✓</span>
              ) : status?.downloading ? (
                <span className="flex shrink-0 items-center gap-1.5 text-sm text-amber-400">
                  <Loader2 className="h-3.5 w-3.5 animate-spin" />
                  {Math.round((status?.download_progress ?? 0) * 100)}%
                </span>
              ) : (
                <button
                  onClick={download}
                  className="flex shrink-0 items-center gap-1.5 rounded bg-blue-600 px-2.5 py-1 text-xs font-medium text-white hover:bg-blue-500"
                >
                  <Download className="h-3.5 w-3.5" /> Télécharger
                </button>
              )}
            </div>
            <p className="text-[10px] text-muted-foreground/60">
              medium = meilleure précision (chiffres, jargon), plus lent. Le worker se met en
              pause si le CPU est chargé, donc ça ne gêne pas le trading.
            </p>
            {status?.error && !status?.downloading && (
              <p className="break-words text-[11px] text-red-400">Erreur : {status.error}</p>
            )}
          </div>

          {/* Mic check */}
          <div className="space-y-2">
            <label className="text-[11px] uppercase tracking-wide text-muted-foreground">
              Microphone
            </label>
            <div className="flex items-center gap-2">
              <select
                value={config?.stt?.input_device ?? "__default__"}
                disabled={monitoring}
                onChange={(e) =>
                  patchStt({ input_device: e.target.value === "__default__" ? null : e.target.value })
                }
                className="min-w-0 flex-1 rounded border border-border bg-background px-2 py-1.5 text-xs text-foreground outline-none disabled:opacity-50"
              >
                <option value="__default__">Périphérique par défaut</option>
                {(devices ?? []).map((d) => (
                  <option key={d} value={d}>
                    {d}
                  </option>
                ))}
              </select>
              <button
                onClick={toggleTest}
                disabled={micBusy}
                className={cn(
                  "flex shrink-0 items-center gap-1.5 rounded px-2.5 py-1.5 text-xs disabled:opacity-50",
                  monitoring
                    ? "bg-red-600 text-white hover:bg-red-500"
                    : "border border-border hover:bg-accent",
                )}
              >
                {monitoring ? <Square className="h-3.5 w-3.5" /> : <Mic className="h-3.5 w-3.5" />}
                {monitoring ? "Arrêter" : "Tester"}
              </button>
            </div>
            {/* Live audiowave — same spectrum as the dictation capture. */}
            {monitoring && (
              <div className="flex h-12 items-end justify-center gap-[2px] rounded-md border border-border bg-background/60 px-2 py-1.5">
                {bins.map((v, i) => (
                  <span
                    key={i}
                    className="w-1 rounded-sm bg-emerald-400"
                    style={{ height: `${Math.max(6, Math.round(v * 100))}%` }}
                  />
                ))}
              </div>
            )}
            <p className="text-[10px] text-muted-foreground/60">
              {monitoring
                ? "Parle : les barres doivent bouger. « Arrêter » coupe le test (aucune note créée)."
                : micBusy
                  ? "Micro occupé par une dictée en cours."
                  : "« Tester » écoute le micro en direct pour vérifier qu'il capte."}
            </p>
          </div>

          {/* Jargon — bias whisper toward trading terms */}
          <div className="space-y-1">
            <label className="text-[11px] uppercase tracking-wide text-muted-foreground">
              Jargon / mots difficiles
            </label>
            <textarea
              value={jargonText}
              onChange={(e) => setJargonText(e.target.value)}
              onBlur={saveJargon}
              rows={4}
              placeholder="halt&#10;VWAP&#10;ticker&#10;float"
              className="w-full resize-y rounded border border-border bg-background px-2 py-1.5 text-xs text-foreground outline-none"
            />
            <p className="text-[10px] text-muted-foreground/60">
              Un terme par ligne. Force whisper sur ces mots (ex. « halt » pas « hault »,
              « ticker » pas « tiqueur »). Français forcé, ces termes EN autorisés. Enregistré
              en quittant le champ.
            </p>
          </div>

          {/* Queue */}
          <div className="space-y-1.5">
            <label className="text-[11px] uppercase tracking-wide text-muted-foreground">
              File ({jobs.length})
            </label>
            <div className="max-h-48 space-y-1.5 overflow-y-auto">
              {jobs.length === 0 && (
                <p className="text-xs text-muted-foreground/50">Aucune dictée en file.</p>
              )}
              {jobs.map((job) => (
                <div
                  key={job.id}
                  className="flex items-start gap-2 rounded-md border border-border/60 bg-background/40 px-2.5 py-1.5"
                >
                  <span
                    className={cn(
                      "mt-0.5 shrink-0 rounded px-1.5 py-0.5 text-[10px] font-semibold",
                      STATE_BADGE[job.state],
                    )}
                  >
                    {STATE_LABEL[job.state]}
                  </span>
                  <div className="min-w-0 flex-1">
                    <p className="text-[11px] text-muted-foreground">
                      {job.kind === "diary" ? "Journal" : `Trade${job.symbol ? ` · ${job.symbol}` : ""}`}
                    </p>
                    {job.text ? (
                      <p className="truncate text-xs text-foreground/80" title={job.text}>
                        {job.text}
                      </p>
                    ) : job.error ? (
                      <p className="truncate text-[11px] text-red-400" title={job.error}>
                        {job.error}
                      </p>
                    ) : null}
                  </div>
                  <div className="flex shrink-0 items-center gap-1">
                    {job.state === "error" && (
                      <button
                        onClick={() => api.sttRetryJob(job.id).then(refresh)}
                        title="Réessayer"
                        className="rounded p-1 text-muted-foreground hover:bg-accent hover:text-foreground"
                      >
                        <RotateCw className="h-3.5 w-3.5" />
                      </button>
                    )}
                    {(job.state === "queued" || job.state === "running") && (
                      <button
                        onClick={() => api.sttCancelJob(job.id).then(refresh)}
                        title="Annuler"
                        className="rounded p-1 text-muted-foreground hover:bg-accent hover:text-foreground"
                      >
                        <X className="h-3.5 w-3.5" />
                      </button>
                    )}
                  </div>
                </div>
              ))}
            </div>
          </div>
        </div>
      </DialogContent>
    </Dialog>
  );
}
