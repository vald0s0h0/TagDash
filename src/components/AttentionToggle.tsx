import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { Radar } from "lucide-react";
import { cn } from "@/lib/utils";
import { attentionApi, ATTENTION_WINDOW_EVENT } from "@/attention/api";
import type { AttentionWindowState } from "@/attention/types";

/** Title-bar switch of the detached « Attention du marché » screen.
 *
 *  One click opens the window (where it was last left — it is meant to sit on a
 *  second monitor), the next closes it. The backend remembers both the state and
 *  the placement, and reopens the window with the app. The button also follows a
 *  close made from the window's own title bar (`attention://window`). */
export function AttentionToggle() {
  const [on, setOn] = useState(false);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    let off: (() => void) | undefined;
    attentionApi.windowState().then((s) => setOn(s.enabled)).catch(() => {});
    listen<AttentionWindowState>(ATTENTION_WINDOW_EVENT, (e) => setOn(e.payload.enabled))
      .then((u) => { off = u; })
      .catch(() => {});
    return () => off?.();
  }, []);

  const toggle = async () => {
    setBusy(true);
    try {
      setOn((await attentionApi.windowSet(!on)).enabled);
    } catch {
      /* the window could not be created: the button stays as it was */
    } finally {
      setBusy(false);
    }
  };

  return (
    <button
      onClick={toggle}
      disabled={busy}
      title={
        on
          ? "Écran Attention du marché — affiché (clic pour le fermer)"
          : "Écran Attention du marché — fenêtre détachée pour un second moniteur (clic pour l'afficher)"
      }
      className={cn(
        "relative flex h-9 w-10 items-center justify-center transition-colors hover:bg-accent",
        on ? "text-emerald-400" : "text-muted-foreground hover:text-foreground",
      )}
    >
      <Radar className="h-4 w-4" />
      {on && <span className="absolute right-2 top-2 h-1.5 w-1.5 rounded-full bg-emerald-400" />}
    </button>
  );
}
