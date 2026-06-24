import { useEffect, useId, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { listen } from "@tauri-apps/api/event";
import { ArrowUp, Mic, Trash2 } from "lucide-react";
import { cn } from "@/lib/utils";
import { api } from "@/lib/tauri";
import { useSttStore } from "@/stores/sttStore";
import type { SttJobKind, SttSpectrum } from "@/types";

// Inline voice-dictation control. The SAME button toggles: click to record (mic
// icon), click again to stop & send (arrow on red) — so the cursor never moves
// between start and stop. While recording a small popover floats just under the
// button with a live audio wave, a timer and a trash (cancel) button. The model
// downloads in the background on first use; the transcription happens async in the
// STT worker (this only captures + enqueues).

const BARS = 18;

interface Props {
  mode: SttJobKind;
  tradeId?: string | null;
  symbol?: string | null;
  /** "card" = inline icon (dashboard journal) ; "toolbar" = chart toolbar pill. */
  variant?: "card" | "toolbar";
  className?: string;
  title?: string;
  /** Disabled (e.g. a trade note with no trade id yet). */
  disabled?: boolean;
  /** Receives a toggle() the parent can fire (e.g. the Xbox gamepad). */
  onRegisterToggle?: (toggle: (() => void) | null) => void;
}

export function MicDictate({
  mode, tradeId, symbol, variant = "card", className, title, disabled, onRegisterToggle,
}: Props) {
  const id = useId();
  const btnRef = useRef<HTMLButtonElement>(null);
  const [recording, setRecording] = useState(false);
  const [bins, setBins] = useState<number[]>(() => new Array(BARS).fill(0));
  const [elapsed, setElapsed] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const [rect, setRect] = useState<DOMRect | null>(null);
  const popRef = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState<{ left: number; top: number } | null>(null);

  const owner = useSttStore((s) => s.owner);
  const claim = useSttStore((s) => s.claim);
  const release = useSttStore((s) => s.release);
  const refresh = useSttStore((s) => s.refresh);
  const busy = owner !== null && owner !== id;

  const liveRef = useRef<{ unlisten?: () => void; timer?: ReturnType<typeof setInterval> }>({});
  const recordingRef = useRef(false);
  useEffect(() => { recordingRef.current = recording; }, [recording]);

  // Position the popover under the button, clamped to the viewport so it never
  // spills off the right (or bottom) edge — the chart toolbar buttons sit near the
  // screen edge. Measured after layout from the popover's real size.
  useLayoutEffect(() => {
    if (!recording || !rect) { setPos(null); return; }
    const margin = 8;
    const pw = popRef.current?.offsetWidth ?? 180;
    const ph = popRef.current?.offsetHeight ?? 40;
    let left = rect.left + rect.width / 2 - pw / 2;
    left = Math.max(margin, Math.min(left, window.innerWidth - pw - margin));
    let top = rect.bottom + 6;
    if (top + ph > window.innerHeight - margin) top = Math.max(margin, rect.top - ph - 6);
    setPos({ left, top });
  }, [recording, rect]);

  const teardown = () => {
    liveRef.current.unlisten?.();
    if (liveRef.current.timer) clearInterval(liveRef.current.timer);
    liveRef.current = {};
  };

  const start = async () => {
    setError(null);
    if (!claim(id)) { setError("Micro occupé par une autre dictée."); return; }
    setRect(btnRef.current?.getBoundingClientRect() ?? null);

    // Kick off the model download in the background if it's not on disk yet.
    const st = await api.sttStatus().catch(() => null);
    if (st && !st.model_present && !st.downloading) api.sttDownloadModel().catch(() => {});

    try {
      await api.sttStartRecording(mode, tradeId ?? null, symbol ?? null);
    } catch (e) {
      setError(String(e));
      release(id);
      return;
    }

    setRecording(true);
    setElapsed(0);
    setBins(new Array(BARS).fill(0));
    const startT = Date.now();
    const timer = setInterval(() => {
      setElapsed(Math.floor((Date.now() - startT) / 1000));
      setRect(btnRef.current?.getBoundingClientRect() ?? null);
    }, 250);
    const un = await listen<SttSpectrum>("stt-spectrum", (e) => setBins(e.payload.bins.slice(0, BARS)));
    liveRef.current = { unlisten: un, timer };
    refresh();
  };

  const finish = (send: boolean) => {
    teardown();
    (send ? api.sttStopRecording() : api.sttCancelRecording())
      .catch(() => {})
      .finally(() => refresh());
    setRecording(false);
    release(id);
  };

  const toggle = () => {
    if (disabled) return;
    if (recordingRef.current) finish(true); // same button → stop & send
    else start();
  };

  // Expose toggle() to the parent (gamepad), and cancel a live recording on unmount
  // so the backend recorder is never left wedged.
  useEffect(() => {
    onRegisterToggle?.(toggle);
    return () => {
      onRegisterToggle?.(null);
      if (recordingRef.current) {
        teardown();
        api.sttCancelRecording().catch(() => {});
        useSttStore.getState().release(id);
      }
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const mm = String(Math.floor(elapsed / 60)).padStart(2, "0");
  const ss = String(elapsed % 60).padStart(2, "0");

  const sizeClass =
    variant === "toolbar"
      ? "flex h-5 shrink-0 items-center justify-center rounded px-1.5"
      : "flex h-7 w-7 items-center justify-center rounded-md";
  const iconSize = variant === "toolbar" ? "h-3 w-3" : "h-4 w-4";

  return (
    <>
      <button
        ref={btnRef}
        type="button"
        title={recording ? "Stop & envoyer" : title ?? "Dicter une note (micro)"}
        disabled={disabled || busy}
        onClick={toggle}
        className={cn(
          sizeClass,
          "transition-colors",
          recording
            ? "bg-red-600 text-white hover:bg-red-500"
            : disabled || busy
              ? "cursor-not-allowed text-muted-foreground/30"
              : "text-muted-foreground hover:bg-accent hover:text-foreground",
          className,
        )}
      >
        {recording ? <ArrowUp className={iconSize} /> : <Mic className={iconSize} />}
      </button>

      {recording &&
        createPortal(
          <div
            ref={popRef}
            style={{
              position: "fixed",
              top: pos?.top ?? -9999,
              left: pos?.left ?? -9999,
              visibility: pos ? "visible" : "hidden",
            }}
            className="z-[2000] flex items-center gap-2 rounded-md border border-border bg-popover px-2 py-1.5 shadow-xl"
          >
            <span className="h-2 w-2 shrink-0 animate-pulse rounded-full bg-red-500" />
            <div className="flex h-6 items-end gap-[2px]">
              {bins.map((v, i) => (
                <span
                  key={i}
                  className="w-[2px] rounded-sm bg-red-400"
                  style={{ height: `${Math.max(8, Math.round(v * 100))}%` }}
                />
              ))}
            </div>
            <span className="font-mono text-[10px] tabular-nums text-muted-foreground">
              {mm}:{ss}
            </span>
            <button
              type="button"
              onClick={() => finish(false)}
              title="Annuler"
              className="rounded p-1 text-muted-foreground hover:bg-accent hover:text-red-400"
            >
              <Trash2 className="h-3.5 w-3.5" />
            </button>
          </div>,
          document.body,
        )}

      {error && !recording &&
        createPortal(
          <div
            style={{
              position: "fixed",
              top: (btnRef.current?.getBoundingClientRect().bottom ?? 0) + 6,
              left: Math.max(
                8,
                Math.min(
                  btnRef.current?.getBoundingClientRect().left ?? 8,
                  window.innerWidth - 248,
                ),
              ),
            }}
            className="z-[2000] max-w-[240px] rounded-md border border-red-700/50 bg-popover px-2 py-1 text-[11px] text-red-400 shadow-xl"
            onClick={() => setError(null)}
          >
            {error}
          </div>,
          document.body,
        )}
    </>
  );
}
