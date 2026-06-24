import { useEffect, useMemo, useRef, useState } from "react";
import { Tag, AlertTriangle, Gamepad2 } from "lucide-react";
import { useGamepadUiStore } from "@/stores/gamepadUiStore";
import { api } from "@/lib/tauri";
import { cn } from "@/lib/utils";

// Controller-driven overlays, mounted once in App. None of these are Radix dialogs
// on purpose — useGamepad suppresses itself while a role="dialog" is open, so
// keeping these as plain overlays lets the pad keep driving them.

// ─── Auto-tag picker (Share button) ───────────────────────────────────────────
// Opens on Share when a live tradeID exists. Each subsequent Share press advances
// the highlight (cycling through the saved tags + "Pas de tag"); 2 s with no press
// confirms and sends the merged tags to TradeTally via the journal pipeline.

function TagPicker({ tradeId, symbol }: { tradeId: string; symbol: string }) {
  const tagAdvance = useGamepadUiStore((s) => s.tagAdvance);
  const close      = useGamepadUiStore((s) => s.closeTagPicker);

  const [tags, setTags] = useState<string[]>([]);
  const [sel, setSel]   = useState(0);
  const [sending, setSending] = useState(false);
  // index 0 = "Pas de tag" (the appui-par-erreur escape hatch).
  const options = useMemo(() => ["Pas de tag", ...tags], [tags]);

  useEffect(() => { api.getJournalTags().then(setTags).catch(() => {}); }, []);

  // Advance the highlight on each Share press (skip the mount value).
  const first = useRef(true);
  useEffect(() => {
    if (first.current) { first.current = false; return; }
    setSel((i) => (i + 1) % options.length);
  }, [tagAdvance, options.length]);

  // Confirm 2 s after the last press; re-armed whenever the selection changes.
  useEffect(() => {
    if (sending) return;
    const t = setTimeout(() => {
      const choice = options[sel];
      if (sel === 0) { close(); return; } // "Pas de tag" → send nothing
      setSending(true);
      (async () => {
        try {
          const entry = await api.getJournalEntry(tradeId);
          const existing = entry?.tags ?? [];
          const merged = existing.includes(choice) ? existing : [...existing, choice];
          await api.saveJournalEntry(tradeId, symbol, entry?.notes ?? "", entry?.confidence ?? null, merged);
        } catch (e) {
          console.error("gamepad tag send failed:", e);
        }
        close();
      })();
    }, 2000);
    return () => clearTimeout(t);
  }, [sel, options.length, sending]); // eslint-disable-line react-hooks/exhaustive-deps

  return (
    <div className="pointer-events-none fixed inset-0 z-[200] flex items-center justify-center">
      <div className="pointer-events-auto w-72 rounded-lg border border-border bg-popover/95 p-4 shadow-2xl backdrop-blur">
        <div className="mb-2 flex items-center gap-2 text-sm font-semibold">
          <Tag className="h-4 w-4 text-blue-400" />
          Tag — {symbol}
        </div>
        <p className="mb-2 text-[11px] text-muted-foreground">
          {sending ? "Envoi à TradeTally…" : "Appuie sur Share pour parcourir · valide après 2 s"}
        </p>
        <div className="max-h-56 space-y-1 overflow-y-auto">
          {options.map((t, i) => (
            <div
              key={t + i}
              className={cn(
                "rounded px-2 py-1.5 text-xs transition-colors",
                i === sel
                  ? "bg-blue-600/30 text-blue-100 ring-1 ring-blue-500/60"
                  : "text-muted-foreground",
                i === 0 && "italic",
              )}
            >
              {t}
            </div>
          ))}
        </div>
        {/* 2 s confirm progress — restarts on each advance via the key. */}
        {!sending && (
          <div className="mt-3 h-1 overflow-hidden rounded bg-border">
            <div key={`${sel}-${tagAdvance}`} className="h-full bg-blue-500 [animation:gamepadTagFill_2s_linear_forwards]" />
          </div>
        )}
      </div>
    </div>
  );
}

// ─── Flash error toast ────────────────────────────────────────────────────────

function FlashError() {
  const msg    = useGamepadUiStore((s) => s.flashError);
  const setMsg = useGamepadUiStore((s) => s.setFlashError);
  useEffect(() => {
    if (!msg) return;
    const t = setTimeout(() => setMsg(null), 1800);
    return () => clearTimeout(t);
  }, [msg, setMsg]);
  if (!msg) return null;
  return (
    <div className="pointer-events-none fixed left-1/2 top-16 z-[200] -translate-x-1/2">
      <div className="flex items-center gap-2 rounded-md border border-red-700/60 bg-red-950/90 px-3 py-2 text-xs text-red-200 shadow-xl">
        <AlertTriangle className="h-3.5 w-3.5" />
        {msg}
      </div>
    </div>
  );
}

// ─── R2 armed-layer hint ──────────────────────────────────────────────────────

function ArmedOverlay() {
  const armed = useGamepadUiStore((s) => s.armed);
  if (!armed) return null;
  return (
    <div className="pointer-events-none fixed bottom-6 left-1/2 z-[200] -translate-x-1/2">
      <div className="flex items-center gap-3 rounded-full border border-orange-600/60 bg-orange-950/90 px-4 py-2 text-xs font-semibold text-orange-100 shadow-xl">
        <span className="flex items-center gap-1.5 text-orange-300">
          <Gamepad2 className="h-4 w-4" /> Position armée
        </span>
        <span className="text-orange-200/90">A&nbsp;25%</span>
        <span className="text-orange-200/90">B&nbsp;50%</span>
        <span className="text-orange-200/90">Y&nbsp;100%</span>
        <span className="text-rose-300">X&nbsp;Clôturer</span>
      </div>
    </div>
  );
}

export function GamepadOverlays() {
  const tag = useGamepadUiStore((s) => s.tag);
  return (
    <>
      {tag && <TagPicker tradeId={tag.tradeId} symbol={tag.symbol} />}
      <FlashError />
      <ArmedOverlay />
    </>
  );
}
