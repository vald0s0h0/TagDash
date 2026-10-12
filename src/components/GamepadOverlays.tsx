import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { Tag, AlertTriangle } from "lucide-react";
import { useGamepadUiStore } from "@/stores/gamepadUiStore";
import { useGamepadStore, clampOverlayScale } from "@/stores/gamepadStore";
import { GamepadPad } from "@/components/GamepadPad";
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

// ─── On-screen pad ────────────────────────────────────────────────────────────
// Shown while the controller is actually driving the app: every button's command
// at the button's place, the LB / LT set swapping in live, and a pressed button
// lighting up. Bottom centre by default; the user drags it out of the way of the
// chart and resizes it from its corner — both are persisted (gamepadStore).
// Double-click puts it back.

const clamp = (v: number, lo: number, hi: number): number => Math.min(Math.max(v, lo), Math.max(lo, hi));

// `pos` / `scale` hold the gesture's latest value, committed on release (a ref, so
// the release never reads a render that is one pointer-move behind).
type PadGesture = { pos?: { x: number; y: number }; scale?: number } & (
  | { kind: "move"; px: number; py: number; cx: number; cy: number; w: number; h: number }
  | { kind: "resize"; cx: number; cy: number; d0: number; s0: number }
);

function PadOverlay() {
  const active  = useGamepadUiStore((s) => s.active);
  const layer   = useGamepadUiStore((s) => s.layer);
  const pressed = useGamepadUiStore((s) => s.pressed);
  const show    = useGamepadStore((s) => s.showOverlay);
  const opacity = useGamepadStore((s) => s.overlayOpacity);
  const savedPos   = useGamepadStore((s) => s.overlayPos);
  const savedScale = useGamepadStore((s) => s.overlayScale);
  const visible = active && show;

  // Live values while a drag / resize is in progress; written to the store (and
  // so to the saved state) once, on release.
  const [livePos, setLivePos]     = useState<{ x: number; y: number } | null>(null);
  const [liveScale, setLiveScale] = useState<number | null>(null);
  const pos   = livePos ?? savedPos;
  const scale = liveScale ?? savedScale;

  const boxRef  = useRef<HTMLDivElement>(null);
  const gesture = useRef<PadGesture | null>(null);

  // The panel's own size, to keep it fully on screen whatever the window size.
  const [box, setBox] = useState({ w: 0, h: 0 });
  useLayoutEffect(() => {
    const el = boxRef.current;
    if (!el) return;
    const measure = () => setBox({ w: el.offsetWidth, h: el.offsetHeight });
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    return () => ro.disconnect();
  }, [visible]);

  if (!visible) return null;

  /** The panel's centre, in px and as window fractions. */
  const centre = () => {
    const r = boxRef.current!.getBoundingClientRect();
    return { cx: r.left + r.width / 2, cy: r.top + r.height / 2, w: r.width, h: r.height };
  };

  const startMove = (e: React.PointerEvent) => {
    if (e.button !== 0 || !boxRef.current) return;
    e.preventDefault();
    boxRef.current.setPointerCapture(e.pointerId);
    gesture.current = { kind: "move", px: e.clientX, py: e.clientY, ...centre() };
  };

  const startResize = (e: React.PointerEvent) => {
    if (e.button !== 0 || !boxRef.current) return;
    e.preventDefault();
    e.stopPropagation();
    boxRef.current.setPointerCapture(e.pointerId);
    const { cx, cy } = centre();
    // Pin the centre first, so the panel grows around a fixed point under the hand.
    const pinned = { x: cx / window.innerWidth, y: cy / window.innerHeight };
    setLivePos(pinned);
    gesture.current = {
      kind: "resize", cx, cy, s0: scale, pos: pinned,
      d0: Math.max(1, Math.hypot(e.clientX - cx, e.clientY - cy)),
    };
  };

  const onMove = (e: React.PointerEvent) => {
    const g = gesture.current;
    if (!g) return;
    if (g.kind === "resize") {
      g.scale = clampOverlayScale(g.s0 * Math.hypot(e.clientX - g.cx, e.clientY - g.cy) / g.d0);
      setLiveScale(g.scale);
      return;
    }
    const dx = e.clientX - g.px;
    const dy = e.clientY - g.py;
    if (!g.pos && Math.hypot(dx, dy) < 3) return; // a click, not a drag
    const W = window.innerWidth;
    const H = window.innerHeight;
    g.pos = {
      x: clamp(g.cx + dx, g.w / 2, W - g.w / 2) / W,
      y: clamp(g.cy + dy, g.h / 2, H - g.h / 2) / H,
    };
    setLivePos(g.pos);
  };

  const end = () => {
    const g = gesture.current;
    if (!g) return;
    gesture.current = null;
    const st = useGamepadStore.getState();
    // A resize that never moved leaves everything as it was (no pinning).
    if (g.pos && (g.kind === "move" || g.scale !== undefined)) st.setOverlayPos(g.pos);
    if (g.scale !== undefined) st.setOverlayScale(g.scale);
    setLivePos(null);
    setLiveScale(null);
  };

  const reset = () => {
    const st = useGamepadStore.getState();
    st.setOverlayPos(null);
    st.setOverlayScale(1);
  };

  return (
    <div
      ref={boxRef}
      onPointerDown={startMove}
      onPointerMove={onMove}
      onPointerUp={end}
      onPointerCancel={end}
      onDoubleClick={reset}
      className="group fixed z-[190] cursor-grab touch-none select-none active:cursor-grabbing"
      style={pos
        ? {
            opacity,
            left: `clamp(${box.w / 2}px, ${pos.x * 100}%, calc(100% - ${box.w / 2}px))`,
            top:  `clamp(${box.h / 2}px, ${pos.y * 100}%, calc(100% - ${box.h / 2}px))`,
            transform: "translate(-50%, -50%)",
          }
        : { opacity, left: "50%", bottom: 16, transform: "translateX(-50%)" }}
    >
      <div className="duration-200 animate-in fade-in slide-in-from-bottom-2">
        <GamepadPad layer={layer} pressed={pressed} size="sm" scale={scale} />
      </div>
      {/* Resize grip — bottom-right corner, revealed on hover. */}
      <div
        onPointerDown={startResize}
        onDoubleClick={(e) => e.stopPropagation()}
        className="absolute -bottom-1 -right-1 flex h-5 w-5 cursor-nwse-resize items-end justify-end opacity-0 transition-opacity group-hover:opacity-100"
      >
        <span className="h-2.5 w-2.5 rounded-br-md border-b-2 border-r-2 border-white/70" />
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
      <PadOverlay />
    </>
  );
}
