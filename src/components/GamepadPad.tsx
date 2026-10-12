import type { HTMLAttributes, ReactNode } from "react";
import { Gamepad2 } from "lucide-react";
import {
  useGamepadStore, PAD, type GamepadCommandId, type GamepadLayer,
} from "@/stores/gamepadStore";
import { resolveDisplay } from "@/lib/gamepadIcons";
import { cn } from "@/lib/utils";

// The controller drawn as a translucent HUD: one key cap per button, placed where
// the button sits on an Xbox pad — D-pad cross on the left, View / Menu in the
// middle, the face-button diamond on the right with RB then RT above it. No button
// names: the position is the legend. LB / LT have no cap — held, they swap every
// cap for that layer's command and their set's name lights up where they sit.
//
// One component for both the on-screen overlay (size "sm") and the Settings
// editor (size "lg" + `edit`), so what's configured is exactly what's shown.

export type PadSize = "sm" | "lg";

interface PadDims {
  kw: number; kh: number; gap: number; mid: number; col: number; pad: number; font: number; icon: number;
}

const PAD_DIMS: Record<PadSize, PadDims> = {
  sm: { kw: 86,  kh: 22, gap: 4, mid: 14, col: 18, pad: 10, font: 10, icon: 11 },
  lg: { kw: 100, kh: 32, gap: 6, mid: 20, col: 24, pad: 16, font: 11, icon: 13 },
};

/** Cap / gap sizes for a pad, optionally scaled (the overlay is user-resizable).
 *  Sizes are recomputed rather than CSS-scaled so the text stays crisp. */
export function padDims(size: PadSize, scale = 1): PadDims {
  const d = PAD_DIMS[size];
  if (scale === 1) return d;
  const k = (v: number) => Math.round(v * scale * 10) / 10;
  return {
    kw: k(d.kw), kh: k(d.kh), gap: k(d.gap), mid: k(d.mid),
    col: k(d.col), pad: k(d.pad), font: k(d.font), icon: k(d.icon),
  };
}

// Each layer has its own tint, so a held modifier is obvious at a glance.
const ACCENT: Record<GamepadLayer, { panel: string; key: string; pressed: string; tag: string; glyph: string }> = {
  base: {
    panel:   "border-white/10",
    key:     "border-white/10 bg-white/[0.07] text-white/85",
    pressed: "border-white bg-white text-zinc-900",
    tag:     "",
    glyph:   "text-white/20",
  },
  lb: {
    panel:   "border-sky-400/40",
    key:     "border-sky-300/25 bg-sky-400/10 text-sky-50",
    pressed: "border-sky-200 bg-sky-400 text-sky-950",
    tag:     "border-sky-400/40 bg-sky-400/15 text-sky-200",
    glyph:   "text-sky-300/80",
  },
  lt: {
    panel:   "border-orange-400/45",
    key:     "border-orange-300/25 bg-orange-400/10 text-orange-50",
    pressed: "border-orange-200 bg-orange-400 text-orange-950",
    tag:     "border-orange-400/40 bg-orange-400/15 text-orange-200",
    glyph:   "text-orange-300/80",
  },
};

// ─── One key cap ──────────────────────────────────────────────────────────────

interface PadKeyProps extends HTMLAttributes<HTMLDivElement> {
  cmd: GamepadCommandId | undefined;
  size: PadSize;
  scale?: number;
  layer?: GamepadLayer;
  pressed?: boolean;
  /** Settings editor: grab cursor, visible empty slots, tooltip. */
  editable?: boolean;
  selected?: boolean;
  /** Settings editor: a dragged command is hovering this slot. */
  over?: boolean;
}

export function PadKey({
  cmd, size, scale, layer = "base", pressed, editable, selected, over, className, style, ...rest
}: PadKeyProps) {
  const override = useGamepadStore((s) => (cmd ? s.display[cmd] : undefined));
  const d = padDims(size, scale);
  const a = ACCENT[layer];
  const r = cmd ? resolveDisplay(cmd, override) : null;
  return (
    <div
      title={editable ? r?.title : undefined}
      className={cn(
        "flex shrink-0 select-none items-center justify-center gap-[0.4em] rounded-[0.5em] border px-[0.5em] font-medium leading-none",
        "transition-[background-color,border-color,color,transform] duration-75",
        r
          ? (pressed ? a.pressed : a.key)
          : pressed
            ? "border-white/40 bg-white/15"
            : cn("border-dashed", editable ? "border-white/20" : "border-white/[0.08]"),
        pressed && "scale-95",
        editable && "cursor-grab touch-none",
        selected && "ring-2 ring-blue-400",
        over && "bg-emerald-400/15 ring-2 ring-emerald-400",
        className,
      )}
      style={{ width: d.kw, height: d.kh, fontSize: d.font, ...style }}
      {...rest}
    >
      {r?.Icon && <r.Icon className="shrink-0" style={{ width: d.icon, height: d.icon }} />}
      {r?.text && <span className="truncate">{r.text}</span>}
    </div>
  );
}

// ─── The pad ──────────────────────────────────────────────────────────────────

export interface PadEdit {
  selected: number | null;
  /** Slot a dragged command is hovering. */
  over: number | null;
  onSlotPointerDown: (button: number, e: React.PointerEvent) => void;
}

/** Four caps in a cross — the D-pad and the face buttons share the shape. */
function Cross({ top, left, right, bottom, gap, mid }: {
  top: ReactNode; left: ReactNode; right: ReactNode; bottom: ReactNode; gap: number; mid: number;
}) {
  return (
    <div className="flex flex-col items-center" style={{ rowGap: gap }}>
      {top}
      <div className="flex" style={{ columnGap: mid }}>{left}{right}</div>
      {bottom}
    </div>
  );
}

export function GamepadPad({
  layer, pressed, size = "sm", scale, edit,
}: {
  layer: GamepadLayer;
  /** Pressed buttons, bit i = button i. */
  pressed: number;
  size?: PadSize;
  scale?: number;
  edit?: PadEdit;
}) {
  const slots = useGamepadStore((s) => s.layout[layer]);
  const names = useGamepadStore((s) => s.layerNames);
  const d = padDims(size, scale);
  const a = ACCENT[layer];

  const key = (button: number) => (
    <PadKey
      cmd={slots[button]}
      size={size}
      scale={scale}
      layer={layer}
      pressed={(pressed & (1 << button)) !== 0}
      editable={!!edit}
      selected={edit?.selected === button}
      over={edit?.over === button}
      data-pad-slot={edit ? button : undefined}
      onPointerDown={edit ? (e) => edit.onSlotPointerDown(button, e) : undefined}
    />
  );

  // Where LT (top) and LB sit: empty at rest, the held set's name otherwise.
  const layerTag = (l: "lt" | "lb") => (
    <div className="flex items-center justify-center" style={{ height: d.kh }}>
      {layer === l && (
        <span
          className={cn(
            "max-w-full truncate rounded-[0.5em] border px-[0.8em] font-semibold uppercase tracking-wider",
            ACCENT[l].tag,
          )}
          style={{ fontSize: d.font * 0.9, lineHeight: `${d.kh * 0.75}px` }}
        >
          {names[l] || (l === "lt" ? "LT" : "LB")}
        </span>
      )}
    </div>
  );

  return (
    <div
      className={cn(
        "inline-flex border bg-zinc-950/65 shadow-[0_8px_32px_rgba(0,0,0,0.35)] backdrop-blur-md",
        "transition-colors duration-150",
        a.panel,
      )}
      style={{ padding: d.pad, columnGap: d.col, borderRadius: d.pad * 1.6 }}
    >
      {/* Left hand: LT / LB (set name only), then the D-pad. */}
      <div className="flex flex-col" style={{ rowGap: d.gap, width: 2 * d.kw + d.mid }}>
        {layerTag("lt")}
        {layerTag("lb")}
        <Cross gap={d.gap} mid={d.mid}
          top={key(PAD.UP)} left={key(PAD.LEFT)} right={key(PAD.RIGHT)} bottom={key(PAD.DOWN)} />
      </div>

      {/* Centre: View · Menu, level with the middle row of both crosses. */}
      <div className="flex flex-col items-center" style={{ rowGap: d.gap }}>
        <div className="flex items-center justify-center" style={{ height: 2 * d.kh + d.gap }}>
          <Gamepad2
            className={cn("transition-colors duration-150", a.glyph)}
            style={{ width: d.kh * 0.8, height: d.kh * 0.8 }}
          />
        </div>
        <div style={{ height: d.kh }} />
        <div className="flex" style={{ columnGap: d.gap }}>
          {key(PAD.VIEW)}
          {key(PAD.MENU)}
        </div>
      </div>

      {/* Right hand: RT above RB, then the face buttons. */}
      <div className="flex flex-col items-center" style={{ rowGap: d.gap, width: 2 * d.kw + d.mid }}>
        {key(PAD.RT)}
        {key(PAD.RB)}
        <Cross gap={d.gap} mid={d.mid}
          top={key(PAD.Y)} left={key(PAD.X)} right={key(PAD.B)} bottom={key(PAD.A)} />
      </div>
    </div>
  );
}
