import { useCallback, useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { X, RotateCcw, Gamepad2, Trash2 } from "lucide-react";
import { Switch } from "@/components/ui/switch";
import {
  useGamepadStore, GAMEPAD_COMMANDS, GAMEPAD_GROUPS, GAMEPAD_AXES, GAMEPAD_LAYERS,
  DEFAULT_AXES, OVERLAY_SCALE_MIN, OVERLAY_SCALE_MAX, PAD, axisLabel, commandDef,
  type GamepadAxisId, type GamepadCommandId, type GamepadDisplay, type GamepadLayer,
} from "@/stores/gamepadStore";
import { useGamepadUiStore } from "@/stores/gamepadUiStore";
import { setGamepadCapturing } from "@/lib/gamepadBus";
import { pickGamepad } from "@/lib/gamepadActions";
import { PAD_ICONS, defaultIconName, resolveDisplay } from "@/lib/gamepadIcons";
import { GamepadPad, PadKey, padDims } from "@/components/GamepadPad";
import { cn } from "@/lib/utils";

// Settings ▸ Manette Xbox. The pad is drawn exactly as the on-screen overlay draws
// it (same component) and edited in place: drop a command on a button, drag it to
// another button, click a button to change its text / icon. Three sets — the base
// one, and the two shown while LB or LT is held. Below: the overlay options, the
// stick axes (recorded by moving a stick), sensitivities and a live read-out.

const LAYER_TABS: Record<GamepadLayer, { label: string; dot: string }> = {
  base: { label: "Base",        dot: "bg-zinc-400" },
  lb:   { label: "LB maintenu", dot: "bg-sky-400" },
  lt:   { label: "LT maintenu", dot: "bg-orange-400" },
};

// Only used to name the selected slot in the inspector — the pad itself carries
// no button names.
const BUTTON_NAMES: Record<number, string> = {
  [PAD.A]: "A", [PAD.B]: "B", [PAD.X]: "X", [PAD.Y]: "Y",
  [PAD.RB]: "RB", [PAD.RT]: "RT", [PAD.VIEW]: "View", [PAD.MENU]: "Menu",
  [PAD.L3]: "Clic stick gauche", [PAD.R3]: "Clic stick droit",
  [PAD.UP]: "D-pad ↑", [PAD.DOWN]: "D-pad ↓", [PAD.LEFT]: "D-pad ←", [PAD.RIGHT]: "D-pad →",
};

const DISPLAY_MODES: { id: NonNullable<GamepadDisplay["mode"]>; label: string }[] = [
  { id: "both", label: "Icône + texte" },
  { id: "icon", label: "Icône" },
  { id: "text", label: "Texte" },
];

function firstConnectedPad(): Gamepad | null {
  return pickGamepad(navigator.getGamepads ? navigator.getGamepads() : []);
}

// ─── Drag & drop (pointer-based) ──────────────────────────────────────────────
// Not HTML5 drag-and-drop: the Tauri webview swallows those events on Windows.

type DragSrc =
  | { from: "palette"; cmd: GamepadCommandId }
  | { from: "slot"; layer: GamepadLayer; button: number; cmd: GamepadCommandId | undefined };

type DropTarget = { kind: "slot"; button: number } | { kind: "palette" } | null;

function targetAt(x: number, y: number): DropTarget {
  const el = document.elementFromPoint(x, y)?.closest<HTMLElement>("[data-pad-slot],[data-pad-palette]");
  if (!el) return null;
  const slot = el.dataset.padSlot;
  return slot !== undefined ? { kind: "slot", button: Number(slot) } : { kind: "palette" };
}

// ─── Small pieces ─────────────────────────────────────────────────────────────

function SectionTitle({ children }: { children: React.ReactNode }) {
  return (
    <div className="text-[10px] font-semibold uppercase tracking-wide text-muted-foreground/60">{children}</div>
  );
}

function Slider({ label, value, min, max, step, format, onChange }: {
  label: string; value: number; min: number; max: number; step: number;
  format: (v: number) => string; onChange: (v: number) => void;
}) {
  return (
    <div className="flex items-center gap-3">
      <span className="w-40 shrink-0 text-xs text-muted-foreground">{label}</span>
      <input
        type="range" min={min} max={max} step={step} value={value}
        onChange={(e) => onChange(Number(e.target.value))}
        className="flex-1 accent-blue-500"
      />
      <span className="w-9 text-right text-xs tabular-nums">{format(value)}</span>
    </div>
  );
}

/** A command in the palette: the interface's icon + its full name. */
function PaletteChip({ cmd, placed, onPointerDown }: {
  cmd: GamepadCommandId;
  /** Already on the set being edited. */
  placed: boolean;
  onPointerDown: (e: React.PointerEvent) => void;
}) {
  const override = useGamepadStore((s) => s.display[cmd]);
  const { Icon } = resolveDisplay(cmd, { ...override, mode: "both" });
  return (
    <div
      onPointerDown={onPointerDown}
      className={cn(
        "flex cursor-grab touch-none select-none items-center gap-1.5 rounded-md border border-border px-2 py-1 text-[11px] transition-colors hover:bg-accent",
        placed ? "text-muted-foreground/50" : "text-foreground/90",
      )}
    >
      {Icon && <Icon className="h-3 w-3 shrink-0" />}
      {commandDef(cmd)?.label}
    </div>
  );
}

// ─── Inspector: the selected button's text / icon ─────────────────────────────

function Inspector({ layer, button, onClose }: { layer: GamepadLayer; button: number; onClose: () => void }) {
  const cmd        = useGamepadStore((s) => s.layout[layer][button]);
  const override   = useGamepadStore((s) => (cmd ? s.display[cmd] : undefined));
  const setDisplay = useGamepadStore((s) => s.setDisplay);
  const assign     = useGamepadStore((s) => s.assign);
  const def = cmd ? commandDef(cmd) : undefined;
  const icon = cmd ? (override?.icon ?? defaultIconName(cmd)) : undefined;
  const mode = override?.mode ?? "both";

  return (
    <div className="rounded-md border border-blue-500/40 bg-blue-500/[0.04] px-3 py-2.5">
      <div className="flex items-center justify-between gap-2">
        <div className="min-w-0 truncate text-xs">
          <span className="font-semibold">{BUTTON_NAMES[button] ?? `Bouton ${button}`}</span>
          <span className="text-muted-foreground"> · {LAYER_TABS[layer].label}</span>
          {def && <span className="text-muted-foreground"> — {def.label}</span>}
        </div>
        <button onClick={onClose} className="text-muted-foreground hover:text-foreground" title="Fermer">
          <X className="h-3.5 w-3.5" />
        </button>
      </div>

      {!cmd || !def ? (
        <p className="mt-1.5 text-[11px] text-muted-foreground">
          Bouton libre — glisse une commande dessus, ou clique une commande ci-dessous.
        </p>
      ) : (
        <div className="mt-2 space-y-2">
          <div className="flex flex-wrap items-center gap-2">
            <input
              value={override?.text ?? ""}
              placeholder={def.short}
              maxLength={18}
              onChange={(e) => setDisplay(cmd, { text: e.target.value })}
              className="h-7 w-44 rounded border border-border bg-background px-2 text-xs outline-none focus:border-blue-500"
            />
            <div className="flex overflow-hidden rounded border border-border">
              {DISPLAY_MODES.map((m) => (
                <button
                  key={m.id}
                  onClick={() => setDisplay(cmd, { mode: m.id })}
                  className={cn(
                    "px-2 py-1 text-[11px] transition-colors",
                    mode === m.id ? "bg-accent text-foreground" : "text-muted-foreground hover:bg-accent/50",
                  )}
                >
                  {m.label}
                </button>
              ))}
            </div>
            <div className="ml-auto flex items-center gap-3">
              <button
                onClick={() => setDisplay(cmd, undefined)}
                disabled={!override}
                className="flex items-center gap-1 text-[11px] text-muted-foreground hover:text-foreground disabled:opacity-30"
                title="Revenir au texte et à l'icône de l'interface"
              >
                <RotateCcw className="h-3 w-3" /> Apparence par défaut
              </button>
              <button
                onClick={() => assign(layer, button, undefined)}
                className="flex items-center gap-1 text-[11px] text-muted-foreground hover:text-red-400"
              >
                <Trash2 className="h-3 w-3" /> Retirer du bouton
              </button>
            </div>
          </div>

          <div className="flex flex-wrap gap-1">
            {Object.entries(PAD_ICONS).map(([name, Icon]) => (
              <button
                key={name}
                onClick={() => setDisplay(cmd, { icon: name })}
                title={name === defaultIconName(cmd) ? "Icône de l'interface" : undefined}
                className={cn(
                  "flex h-6 w-6 items-center justify-center rounded border transition-colors",
                  icon === name
                    ? "border-blue-500 bg-blue-500/15 text-foreground"
                    : "border-transparent text-muted-foreground hover:bg-accent hover:text-foreground",
                )}
              >
                <Icon className="h-3.5 w-3.5" />
              </button>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}

// ─── Panel ────────────────────────────────────────────────────────────────────

export function GamepadSettings() {
  const enabled       = useGamepadStore((s) => s.enabled);
  const setEnabled    = useGamepadStore((s) => s.setEnabled);
  const layout        = useGamepadStore((s) => s.layout);
  const assign        = useGamepadStore((s) => s.assign);
  const moveSlot      = useGamepadStore((s) => s.moveSlot);
  const layerNames    = useGamepadStore((s) => s.layerNames);
  const setLayerName  = useGamepadStore((s) => s.setLayerName);
  const axes          = useGamepadStore((s) => s.axes);
  const setAxis       = useGamepadStore((s) => s.setAxis);
  const resetDefaults = useGamepadStore((s) => s.resetDefaults);
  const showOverlay       = useGamepadStore((s) => s.showOverlay);
  const setShowOverlay    = useGamepadStore((s) => s.setShowOverlay);
  const overlayOpacity    = useGamepadStore((s) => s.overlayOpacity);
  const setOverlayOpacity = useGamepadStore((s) => s.setOverlayOpacity);
  const overlayScale      = useGamepadStore((s) => s.overlayScale);
  const setOverlayScale   = useGamepadStore((s) => s.setOverlayScale);
  const overlayPos        = useGamepadStore((s) => s.overlayPos);
  const setOverlayPos     = useGamepadStore((s) => s.setOverlayPos);
  const leftSens      = useGamepadStore((s) => s.leftSensitivity);
  const rightSens     = useGamepadStore((s) => s.rightSensitivity);
  const setLeftSens   = useGamepadStore((s) => s.setLeftSensitivity);
  const setRightSens  = useGamepadStore((s) => s.setRightSensitivity);
  const invertZoomTime  = useGamepadStore((s) => s.invertZoomTime);
  const invertZoomPrice = useGamepadStore((s) => s.invertZoomPrice);
  const invertCursor    = useGamepadStore((s) => s.invertCursor);
  const setInvertZoomTime  = useGamepadStore((s) => s.setInvertZoomTime);
  const setInvertZoomPrice = useGamepadStore((s) => s.setInvertZoomPrice);
  const setInvertCursor    = useGamepadStore((s) => s.setInvertCursor);

  // Live pad state, published by the global loop (it keeps reading the pad while
  // this dialog suppresses the commands).
  const connected = useGamepadUiStore((s) => s.connected);
  const padLayer  = useGamepadUiStore((s) => s.layer);
  const pressed   = useGamepadUiStore((s) => s.pressed);

  // The set being edited: the chosen tab — or, while LB / LT is held on the pad,
  // that set (same gesture as in use).
  const [tab, setTab] = useState<GamepadLayer>("base");
  const layer = padLayer !== "base" ? padLayer : tab;
  const layerRef = useRef(layer);
  layerRef.current = layer;

  const [selected, setSelected] = useState<number | null>(null);
  const selectedRef = useRef(selected);
  selectedRef.current = selected;

  // ── Drag & drop ────────────────────────────────────────────────────────────
  const [drag, setDrag] = useState<{ cmd: GamepadCommandId; x: number; y: number } | null>(null);
  const [over, setOver] = useState<DropTarget>(null);
  const dragCleanup = useRef<(() => void) | null>(null);
  useEffect(() => () => dragCleanup.current?.(), []);

  const drop = useCallback((src: DragSrc, target: DropTarget) => {
    if (!src.cmd || !target) return;
    const dst = layerRef.current;
    if (target.kind === "palette") {
      if (src.from === "slot") assign(src.layer, src.button, undefined); // dragged back = removed
      return;
    }
    if (src.from === "slot" && src.layer === dst) moveSlot(dst, src.button, target.button);
    else assign(dst, target.button, src.cmd); // from the palette, or copied across sets
    setSelected(target.button);
  }, [assign, moveSlot]);

  const beginDrag = useCallback((src: DragSrc, e: React.PointerEvent) => {
    if (e.button !== 0) return;
    e.preventDefault(); // no text selection while dragging
    const x0 = e.clientX;
    const y0 = e.clientY;
    let moved = false;

    const onMove = (ev: PointerEvent) => {
      if (!src.cmd) return; // an empty slot can only be clicked
      if (!moved && Math.hypot(ev.clientX - x0, ev.clientY - y0) < 5) return;
      moved = true;
      setDrag({ cmd: src.cmd, x: ev.clientX, y: ev.clientY });
      setOver(targetAt(ev.clientX, ev.clientY));
    };
    const end = () => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
      window.removeEventListener("pointercancel", end);
      dragCleanup.current = null;
      setDrag(null);
      setOver(null);
    };
    const onUp = (ev: PointerEvent) => {
      end();
      if (moved) { drop(src, targetAt(ev.clientX, ev.clientY)); return; }
      // A plain click: select the button — or send the command to the selected one.
      if (src.from === "slot") setSelected((s) => (s === src.button ? null : src.button));
      else if (selectedRef.current !== null) assign(layerRef.current, selectedRef.current, src.cmd);
    };
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp);
    window.addEventListener("pointercancel", end);
    dragCleanup.current = end;
  }, [assign, drop]);

  const onSlotPointerDown = (button: number, e: React.PointerEvent) =>
    beginDrag({ from: "slot", layer, button, cmd: layout[layer][button] }, e);

  const overSlot = over?.kind === "slot" ? over.button : null;
  const edit = { selected, over: overSlot, onSlotPointerDown };
  const placed = new Set(Object.values(layout[layer]));

  // ── Axis recorder — armed while `recording` is set ─────────────────────────
  const [recording, setRecording] = useState<GamepadAxisId | null>(null);
  const [reject, setReject] = useState<string | null>(null);
  useEffect(() => {
    if (!recording) return;
    setGamepadCapturing(true);
    let restAxes: number[] | null = null;
    let raf = 0;
    const timeout = window.setTimeout(() => stop(), 10_000); // safety: don't hang armed

    const stop = () => {
      window.clearTimeout(timeout);
      cancelAnimationFrame(raf);
      setRecording(null);
      setGamepadCapturing(false);
    };

    const tick = () => {
      const pad = firstConnectedPad();
      if (pad) {
        if (restAxes === null) restAxes = [...pad.axes];
        for (let i = 0; i < pad.axes.length; i++) {
          if (Math.abs(pad.axes[i] - (restAxes[i] ?? 0)) > 0.6) {
            setAxis(recording, i); setReject(null); stop(); return;
          }
        }
        if (pad.buttons.some((b) => b.pressed || b.value > 0.6)) {
          setReject("Ce bouton ne renvoie pas de valeur continue — bouge un stick.");
        }
      }
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);

    const onKey = (e: KeyboardEvent) => { if (e.key === "Escape") { e.preventDefault(); stop(); } };
    window.addEventListener("keydown", onKey, true);
    return () => { window.clearTimeout(timeout); cancelAnimationFrame(raf); setGamepadCapturing(false); window.removeEventListener("keydown", onKey, true); };
  }, [recording, setAxis]);

  // ── Live stick read-out (throttled ~8 fps) + raw API diagnostics ───────────
  const [liveAxes, setLiveAxes] = useState<number[]>([]);
  const [diag, setDiag] = useState<{ present: number; slots: number; id: string; mapping: string; nb: number; na: number }>(
    { present: 0, slots: 0, id: "", mapping: "", nb: 0, na: 0 },
  );
  useEffect(() => {
    let raf = 0;
    let last = 0;
    const tick = (t: number) => {
      if (t - last > 120) {
        last = t;
        // Raw API state — reveals whether navigator.getGamepads() returns anything
        // at all and which backend (gilrs polyfill ≈ 20 btns / native ≈ 17).
        const raw = navigator.getGamepads ? navigator.getGamepads() : [];
        const any = [...raw].find((p) => p) ?? null;
        const pad = firstConnectedPad();
        setDiag({
          present: [...raw].filter((p) => p).length,
          slots: raw.length,
          id: (pad ?? any)?.id ?? "",
          mapping: (pad ?? any)?.mapping ?? "—",
          nb: (pad ?? any)?.buttons.length ?? 0,
          na: (pad ?? any)?.axes.length ?? 0,
        });
        setLiveAxes(pad ? [...pad.axes] : []);
      }
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  }, []);

  const d = padDims("lg");

  return (
    <div className="space-y-3">
      {/* Enable + status + reset */}
      <div className="flex items-center justify-between rounded-md border border-border px-3 py-2.5">
        <div className="min-w-0">
          <div className="flex items-center gap-2 text-sm font-medium">
            <Gamepad2 className="h-4 w-4 text-muted-foreground" />
            Manette Xbox
            <span className={cn(
              "rounded px-1.5 py-0.5 text-[9px] font-semibold uppercase",
              connected ? "bg-emerald-900/50 text-emerald-300" : "bg-zinc-800 text-muted-foreground",
            )}>
              {connected ? "connectée" : "absente"}
            </span>
          </div>
          <p className="mt-0.5 text-xs text-muted-foreground">
            Active dès qu'une manette est branchée (USB-C) ou appairée (Bluetooth, macOS).
          </p>
        </div>
        <div className="flex items-center gap-3">
          <button
            onClick={() => { resetDefaults(); setSelected(null); }}
            title="Réinitialiser la disposition, les textes et les icônes par défaut"
            className="flex items-center gap-1 text-[11px] text-muted-foreground hover:text-foreground"
          >
            <RotateCcw className="h-3 w-3" /> Défauts
          </button>
          <Switch checked={enabled} onCheckedChange={setEnabled} />
        </div>
      </div>

      {/* ── Layout editor ── */}
      <div className="space-y-2">
        <div className="flex flex-wrap items-center gap-2">
          <div className="flex overflow-hidden rounded-md border border-border">
            {GAMEPAD_LAYERS.map((l) => (
              <button
                key={l}
                onClick={() => setTab(l)}
                className={cn(
                  "flex items-center gap-1.5 px-3 py-1.5 text-xs transition-colors",
                  layer === l ? "bg-accent text-foreground" : "text-muted-foreground hover:bg-accent/50",
                )}
              >
                <span className={cn("h-1.5 w-1.5 rounded-full", LAYER_TABS[l].dot)} />
                {LAYER_TABS[l].label}
              </button>
            ))}
          </div>
          {layer !== "base" && (
            <label className="flex items-center gap-2 text-[11px] text-muted-foreground">
              Nom du set
              <input
                value={layerNames[layer]}
                maxLength={22}
                onChange={(e) => setLayerName(layer, e.target.value)}
                className="h-7 w-40 rounded border border-border bg-background px-2 text-xs text-foreground outline-none focus:border-blue-500"
              />
            </label>
          )}
        </div>
        <p className="text-[11px] leading-snug text-muted-foreground">
          LB et LT ne portent pas de commande : maintenus, ils font apparaître un autre set de
          boutons (comme Ctrl ou Alt). Glisse une commande sur un bouton, déplace-la d'un bouton
          à l'autre, ou clique un bouton pour changer son texte et son icône. Seuls les boutons
          du set affiché agissent — un bouton libre ne fait rien.
        </p>

        {/* The pad, as it shows on screen — over a chart-like backdrop. */}
        <div
          className="flex flex-col items-center gap-3 rounded-lg border border-border bg-zinc-900 px-3 py-5"
          style={{
            backgroundImage:
              "linear-gradient(rgba(255,255,255,0.035) 1px, transparent 1px), linear-gradient(90deg, rgba(255,255,255,0.035) 1px, transparent 1px)",
            backgroundSize: "32px 32px",
          }}
        >
          <div style={{ opacity: overlayOpacity }}>
            <GamepadPad layer={layer} pressed={pressed} size="lg" edit={edit} />
          </div>
          {/* Stick clicks: usable, but not part of the on-screen pad. */}
          <div className="flex items-center gap-6 text-[10px] text-white/45">
            {([[PAD.L3, "Clic stick gauche"], [PAD.R3, "Clic stick droit"]] as const).map(([button, label]) => (
              <div key={button} className="flex items-center gap-2">
                {label}
                <PadKey
                  cmd={layout[layer][button]}
                  size="lg"
                  layer={layer}
                  pressed={(pressed & (1 << button)) !== 0}
                  editable
                  selected={selected === button}
                  over={overSlot === button}
                  data-pad-slot={button}
                  onPointerDown={(e) => onSlotPointerDown(button, e)}
                  style={{ height: d.kh - 6 }}
                />
              </div>
            ))}
          </div>
        </div>

        {selected !== null && (
          <Inspector layer={layer} button={selected} onClose={() => setSelected(null)} />
        )}

        {/* Command palette — also the drop zone that removes a command. */}
        <div
          data-pad-palette
          className={cn(
            "space-y-2 rounded-md border px-3 py-2.5 transition-colors",
            over?.kind === "palette" ? "border-red-500/60 bg-red-500/[0.06]" : "border-border",
          )}
        >
          {GAMEPAD_GROUPS.map((group) => (
            <div key={group} className="flex items-start gap-3">
              <span className="w-32 shrink-0 pt-1 text-[10px] font-semibold uppercase tracking-wide text-muted-foreground/60">
                {group}
              </span>
              <div className="flex flex-wrap gap-1.5">
                {GAMEPAD_COMMANDS.filter((c) => c.group === group).map((c) => (
                  <PaletteChip
                    key={c.id}
                    cmd={c.id}
                    placed={placed.has(c.id)}
                    onPointerDown={(e) => beginDrag({ from: "palette", cmd: c.id }, e)}
                  />
                ))}
              </div>
            </div>
          ))}
          <p className="text-[10px] leading-tight text-muted-foreground/60">
            Une commande peut figurer sur plusieurs sets. Grisée = déjà sur ce set. Pour la retirer
            d'un bouton, ramène-la ici.
          </p>
        </div>
      </div>

      {/* ── On-screen overlay ── */}
      <div className="space-y-2 rounded-md border border-border px-3 py-2.5">
        <div className="flex items-center justify-between">
          <div>
            <div className="text-xs font-medium">Afficher la manette à l'écran</div>
            <p className="mt-0.5 text-[11px] text-muted-foreground">
              Tant que la manette pilote le chart. Glisse-la à la souris pour la déplacer, tire
              son coin bas-droit pour la redimensionner, double-clic pour la replacer — sa
              position et sa taille sont mémorisées.
            </p>
          </div>
          <Switch checked={showOverlay} onCheckedChange={setShowOverlay} />
        </div>
        <Slider
          label="Opacité" value={overlayOpacity} min={0.4} max={1} step={0.05}
          format={(v) => `${Math.round(v * 100)}%`} onChange={setOverlayOpacity}
        />
        <Slider
          label="Taille" value={overlayScale} min={OVERLAY_SCALE_MIN} max={OVERLAY_SCALE_MAX} step={0.05}
          format={(v) => `${Math.round(v * 100)}%`} onChange={setOverlayScale}
        />
        <button
          onClick={() => { setOverlayPos(null); setOverlayScale(1); }}
          disabled={overlayPos === null && overlayScale === 1}
          className="flex items-center gap-1 text-[11px] text-muted-foreground hover:text-foreground disabled:opacity-30"
        >
          <RotateCcw className="h-3 w-3" /> Replacer en bas au centre, taille d'origine
        </button>
      </div>

      {/* ── Sticks: axes, sensitivity, live read-out ── */}
      <div className="space-y-2 rounded-md border border-border px-3 py-2.5">
        <div className="flex items-center justify-between">
          <SectionTitle>Sticks</SectionTitle>
          <span className="font-mono text-[10px] text-muted-foreground/70" title={diag.id}>
            API: {diag.present}/{diag.slots} · map={diag.mapping} · {diag.nb}b/{diag.na}a
          </span>
        </div>

        {GAMEPAD_AXES.map((a) => (
          <div key={a.id} className="flex items-center justify-between gap-2">
            <span className="text-xs">{a.label}</span>
            <div className="flex items-center gap-1.5">
              <button
                onClick={() => { setReject(null); setRecording((r) => (r === a.id ? null : a.id)); }}
                className={cn(
                  "min-w-[7.5rem] rounded border px-2 py-1 text-center font-mono text-[11px] transition-colors",
                  recording === a.id
                    ? "animate-pulse border-blue-500 text-blue-300"
                    : "border-border text-foreground/80 hover:bg-accent",
                )}
              >
                {recording === a.id ? "Bouge un stick…" : axisLabel(axes[a.id])}
              </button>
              <button
                onClick={() => setAxis(a.id, DEFAULT_AXES[a.id])}
                disabled={recording === a.id}
                className="text-muted-foreground hover:text-red-400 disabled:opacity-20"
                title="Réassigner par défaut"
              >
                <X className="h-3 w-3" />
              </button>
            </div>
          </div>
        ))}
        {reject && (
          <div className="rounded-md border border-amber-700/50 bg-amber-950/40 px-3 py-1.5 text-[11px] text-amber-200">
            {reject}
          </div>
        )}

        <div className="grid grid-cols-2 gap-x-4 gap-y-1 pt-1 text-[11px]">
          {/* W3C standard axis indices: 0=LX 1=LY 2=RX 3=RY. */}
          {([[0, "Stick G ↔"], [1, "Stick G ↕"], [2, "Stick D ↔"], [3, "Stick D ↕"]] as const).map(([idx, label]) => {
            const v = liveAxes[idx] ?? 0;
            return (
              <div key={idx} className="flex items-center gap-2">
                <span className="w-20 shrink-0 text-muted-foreground">{label}</span>
                <div className="relative h-1.5 flex-1 rounded bg-border">
                  <div
                    className="absolute top-0 h-full rounded bg-sky-500"
                    style={{ left: "50%", width: `${Math.abs(v) * 50}%`, transform: v < 0 ? "translateX(-100%)" : "none" }}
                  />
                </div>
              </div>
            );
          })}
        </div>

        <div className="space-y-2 pt-1">
          <Slider label="Stick gauche (zoom)" value={leftSens} min={0.2} max={3} step={0.1}
            format={(v) => `${v.toFixed(1)}×`} onChange={setLeftSens} />
          <Slider label="Stick droit (curseur)" value={rightSens} min={0.2} max={3} step={0.1}
            format={(v) => `${v.toFixed(1)}×`} onChange={setRightSens} />
          <div className="flex flex-wrap gap-4 pt-1">
            {([
              ["Inverser zoom horizontal", invertZoomTime, setInvertZoomTime],
              ["Inverser zoom vertical",   invertZoomPrice, setInvertZoomPrice],
              ["Inverser curseur",         invertCursor, setInvertCursor],
            ] as const).map(([label, val, set]) => (
              <label key={label} className="flex items-center gap-2 text-[11px] text-muted-foreground">
                <Switch checked={val} onCheckedChange={set} />
                {label}
              </label>
            ))}
          </div>
        </div>
      </div>

      {/* Drag ghost — the cap as it will look on the pad. */}
      {drag && createPortal(
        <div
          className="pointer-events-none fixed z-[300] rounded-md bg-zinc-900 shadow-xl"
          style={{ left: drag.x, top: drag.y, transform: "translate(-50%, -50%)" }}
        >
          <PadKey cmd={drag.cmd} size="lg" layer={layer} />
        </div>,
        document.body,
      )}
    </div>
  );
}
