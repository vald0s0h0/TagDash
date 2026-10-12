import { create } from "zustand";
import { persist } from "zustand/middleware";
import { userStorage } from "@/lib/userStorage";

// Xbox-controller layout + tuning, persisted with the personal data (same pattern as
// hotkeyStore). The whole point of persisting is that the maintainer's chosen
// layout/sensitivities become the bundled distribution defaults (DEFAULT_LAYOUT
// below). The controller is auto-active the moment the OS enumerates it (Web
// Gamepad API over USB-C or Bluetooth) — the `enabled` switch is just an opt-out.
//
// Two command kinds:
//   • digital — a single button press (edge-triggered, like a keyboard chord).
//   • analog  — a stick AXIS returning a continuous −1..+1 value (zoom / cursor).
//
// Digital commands live in LAYERS, exactly like keyboard modifiers: LB and LT are
// never commands themselves — held, each one swaps the whole pad for another set
// of buttons (Ctrl / Alt style). So a layout is `layer → button → command`, and a
// command may sit on several layers at once. Layers are strict: while a modifier
// is held only that layer's buttons fire, an empty slot does nothing — what the
// on-screen overlay shows is exactly what the pad does.

// ─── Bindable commands ────────────────────────────────────────────────────────

export type GamepadAxisId = "zoom_time" | "zoom_price" | "cursor_move";

export type GamepadCommandId =
  // Navigation
  | "ticker_prev" | "ticker_next" | "ticker_release"
  | "tab_prev" | "tab_next" | "focus_cycle"
  // Levels at the cursor
  | "cursor_sl" | "cursor_alarm" | "cursor_tp" | "remove_orders"
  // Position
  | "order_25" | "order_50" | "order_100" | "close_position"
  | "confirm_hod" | "invert_hod"
  // Journal / TradeTally
  | "capture" | "share_tag" | "journal_audio"
  // Replay (global — not tied to a zone)
  | "replay_next_alert" | "replay_play_pause";

export type GamepadActionId = GamepadCommandId | GamepadAxisId;

export type GamepadGroup =
  | "Navigation" | "Curseur / Ordres" | "Position"
  | "Journal / TradeTally" | "Replay";

export interface GamepadCommandDef {
  id: GamepadCommandId;
  /** Full name (Settings palette, tooltips). */
  label: string;
  /** Default text on the key cap — short enough for the on-screen overlay. */
  short: string;
  group: GamepadGroup;
}

export const GAMEPAD_COMMANDS: GamepadCommandDef[] = [
  // Navigation
  { id: "ticker_prev",    label: "Ticker précédent",          short: "Précédent",    group: "Navigation" },
  { id: "ticker_next",    label: "Ticker suivant",            short: "Suivant",      group: "Navigation" },
  { id: "ticker_release", label: "Libérer / supprimer",       short: "Libérer",      group: "Navigation" },
  { id: "tab_prev",       label: "Onglet précédent",          short: "Onglet −",     group: "Navigation" },
  { id: "tab_next",       label: "Onglet suivant",            short: "Onglet +",     group: "Navigation" },
  { id: "focus_cycle",    label: "Changer le focus du chart", short: "Focus",        group: "Navigation" },

  // Levels at the cursor (right stick)
  { id: "cursor_sl",     label: "Stop Loss au curseur",   short: "Stop Loss",   group: "Curseur / Ordres" },
  { id: "cursor_alarm",  label: "Alarme au curseur",      short: "Alarme",      group: "Curseur / Ordres" },
  { id: "cursor_tp",     label: "Take Profit au curseur", short: "Take Profit", group: "Curseur / Ordres" },
  { id: "remove_orders", label: "Retirer les ordres (double = + alarmes)", short: "Retirer", group: "Curseur / Ordres" },

  // Position
  { id: "order_25",       label: "Position 25 %",              short: "25 %",      group: "Position" },
  { id: "order_50",       label: "Position 50 %",              short: "50 %",      group: "Position" },
  { id: "order_100",      label: "Position 100 %",             short: "100 %",     group: "Position" },
  { id: "close_position", label: "Clôturer la position",       short: "Clôturer",  group: "Position" },
  { id: "confirm_hod",    label: "Confirmer HOD Drive",        short: "HOD Drive", group: "Position" },
  { id: "invert_hod",     label: "Inverser HOD Drive (short)", short: "HOD short", group: "Position" },

  // Journal / TradeTally
  { id: "capture",       label: "Capture TradeTally",          short: "Capture",    group: "Journal / TradeTally" },
  { id: "share_tag",     label: "Ajouter un tag (TradeTally)", short: "Tag",        group: "Journal / TradeTally" },
  { id: "journal_audio", label: "Note audio (dictée trade)",   short: "Note audio", group: "Journal / TradeTally" },

  // Replay (global — pas de zone ciblée)
  { id: "replay_play_pause", label: "Replay : lecture / pause",  short: "Lecture",  group: "Replay" },
  { id: "replay_next_alert", label: "Replay : prochaine alerte", short: "Alerte +", group: "Replay" },
];

export const GAMEPAD_GROUPS: GamepadGroup[] = [
  "Navigation", "Curseur / Ordres", "Position", "Journal / TradeTally", "Replay",
];

export function commandDef(id: GamepadCommandId): GamepadCommandDef | undefined {
  return GAMEPAD_COMMANDS.find((c) => c.id === id);
}

export const GAMEPAD_AXES: { id: GamepadAxisId; label: string }[] = [
  { id: "zoom_time",   label: "Zoom horizontal (temps)" },
  { id: "zoom_price",  label: "Zoom vertical (prix)" },
  { id: "cursor_move", label: "Curseur horizontal" },
];

// ─── Buttons & layers ─────────────────────────────────────────────────────────
// W3C "standard" mapping (native Gamepad API): A=0 B=1 X=2 Y=3 · LB=4 RB=5 LT=6
// RT=7 · View=8 Menu=9 L3=10 R3=11 · D-pad 12↑/13↓/14←/15→ · Guide=16 Share=17 ·
// axes LX=0 LY=1 RX=2 RY=3. PS names map to Xbox: L1=LB L2=LT R1=RB R2=RT.

export const PAD = {
  A: 0, B: 1, X: 2, Y: 3,
  LB: 4, RB: 5, LT: 6, RT: 7,
  VIEW: 8, MENU: 9, L3: 10, R3: 11,
  UP: 12, DOWN: 13, LEFT: 14, RIGHT: 15,
} as const;

export type GamepadLayer = "base" | "lb" | "lt";
export type GamepadModLayer = Exclude<GamepadLayer, "base">;

export const GAMEPAD_LAYERS: GamepadLayer[] = ["base", "lb", "lt"];

/** The button that, held, switches the pad to a layer. Not assignable. */
export const LAYER_MODIFIER: Record<GamepadModLayer, number> = { lb: PAD.LB, lt: PAD.LT };

export function isModifierButton(index: number): boolean {
  return index === PAD.LB || index === PAD.LT;
}

const AXIS_LABELS: Record<number, string> = {
  0: "Stick gauche ↔", 1: "Stick gauche ↕",
  2: "Stick droit ↔",  3: "Stick droit ↕",
};

export function axisLabel(index: number | undefined): string {
  if (index === undefined) return "non assigné";
  return AXIS_LABELS[index] ?? `Axe ${index}`;
}

// ─── Layout model ─────────────────────────────────────────────────────────────

/** `layer → button index → command`. A missing key is an empty slot. */
export type GamepadLayout = Record<GamepadLayer, Partial<Record<number, GamepadCommandId>>>;
/** `undefined` = left for the user to assign. */
export type GamepadAxes = Record<GamepadAxisId, number | undefined>;

/** How a command is drawn on its key cap (overlay + Settings). Per command, so it
 *  follows the command wherever it's moved. Every field falls back to the default
 *  in `gamepadIcons` (the interface's own icon) / the command's `short` text. */
export interface GamepadDisplay {
  mode?: "both" | "icon" | "text";
  text?: string;
  /** Icon name from the `PAD_ICONS` registry. */
  icon?: string;
}

// ─── Default layout (= distribution defaults) ─────────────────────────────────
// LT held is the "armed" set: the face buttons become the position sizes, so an
// order can never fire from a bare press. LB held carries the session tabs.

export const DEFAULT_LAYOUT: GamepadLayout = {
  base: {
    [PAD.UP]:    "ticker_prev",
    [PAD.DOWN]:  "ticker_next",
    [PAD.LEFT]:  "ticker_release",
    [PAD.RIGHT]: "share_tag",
    [PAD.A]: "cursor_sl",
    [PAD.B]: "cursor_alarm",
    [PAD.Y]: "cursor_tp",
    [PAD.X]: "remove_orders",
    [PAD.VIEW]: "capture",
    [PAD.MENU]: "journal_audio",
    [PAD.RB]: "focus_cycle",
    [PAD.RT]: "tab_next",
    [PAD.R3]: "replay_next_alert",
    [PAD.L3]: "replay_play_pause",
  },
  lb: {
    [PAD.LEFT]:  "tab_prev",
    [PAD.RIGHT]: "tab_next",
  },
  // invert_hod is deliberately left out: firing a short by accident is the worst
  // possible misbind — the user drops it on a button themselves in Settings.
  lt: {
    [PAD.A]: "order_25",
    [PAD.B]: "order_50",
    [PAD.Y]: "order_100",
    [PAD.X]: "close_position",
    [PAD.RIGHT]: "confirm_hod",
  },
};

export const DEFAULT_AXES: GamepadAxes = {
  zoom_time:   0, // left stick X
  zoom_price:  1, // left stick Y
  cursor_move: 3, // right stick Y
};

export const DEFAULT_LAYER_NAMES: Record<GamepadModLayer, string> = {
  lb: "Navigation",
  lt: "Position armée",
};

function cloneLayout(l: GamepadLayout): GamepadLayout {
  return { base: { ...l.base }, lb: { ...l.lb }, lt: { ...l.lt } };
}

// ─── Migration from the pre-layer model (persist v ≤ 4) ───────────────────────
// Before v5 a binding was `command → button`, with R2/RT as the single held
// modifier ("armed" commands). Custom layouts are carried over: armed commands go
// to the LT layer, the rest to the base layer. LB/LT themselves are modifiers now,
// so whatever sat on them is dropped (the session tabs get their new defaults).

type LegacyBinding = { kind: "button" | "axis"; index: number } | undefined;
const LEGACY_ARMED: GamepadCommandId[] = [
  "order_25", "order_50", "order_100", "close_position", "confirm_hod", "invert_hod",
];

function migrateLegacy(bindings: Record<string, LegacyBinding>): { layout: GamepadLayout; axes: GamepadAxes } {
  const layout: GamepadLayout = { base: {}, lb: { ...DEFAULT_LAYOUT.lb }, lt: {} };
  for (const c of GAMEPAD_COMMANDS) {
    const b = bindings[c.id];
    if (!b || b.kind !== "button" || isModifierButton(b.index)) continue;
    const layer = LEGACY_ARMED.includes(c.id) ? layout.lt : layout.base;
    if (layer[b.index] === undefined) layer[b.index] = c.id; // first command wins, as before
  }
  // RT was the modifier, so it's free: give it its new default.
  if (layout.base[PAD.RT] === undefined) layout.base[PAD.RT] = DEFAULT_LAYOUT.base[PAD.RT];
  // …and L3 gets replay play / pause when nothing was carried onto it.
  if (layout.base[PAD.L3] === undefined) layout.base[PAD.L3] = DEFAULT_LAYOUT.base[PAD.L3];
  const axes = { ...DEFAULT_AXES };
  for (const a of GAMEPAD_AXES) {
    const b = bindings[a.id];
    if (b?.kind === "axis") axes[a.id] = b.index;
  }
  return { layout, axes };
}

// ─── Persisted store ──────────────────────────────────────────────────────────

export const OVERLAY_SCALE_MIN = 0.7;
export const OVERLAY_SCALE_MAX = 2;

export function clampOverlayScale(v: number): number {
  return Math.min(OVERLAY_SCALE_MAX, Math.max(OVERLAY_SCALE_MIN, v));
}

interface GamepadState {
  enabled: boolean;
  layout: GamepadLayout;
  axes: GamepadAxes;
  display: Partial<Record<GamepadCommandId, GamepadDisplay>>;
  /** Titles shown on the overlay while LB / LT is held. */
  layerNames: Record<GamepadModLayer, string>;
  /** On-screen pad overlay while the controller is active. */
  showOverlay: boolean;
  overlayOpacity: number; // 0.4 … 1
  /** Where the user dragged the overlay: its centre, as fractions of the window
   *  (so it keeps its place across window sizes). Null = bottom centre. */
  overlayPos: { x: number; y: number } | null;
  /** Overlay size multiplier (OVERLAY_SCALE_MIN … OVERLAY_SCALE_MAX). */
  overlayScale: number;
  /** Stick speed multipliers (0.2 = slow … 3 = fast). */
  leftSensitivity:  number; // zoom (left stick)
  rightSensitivity: number; // cursor (right stick)
  invertZoomTime:  boolean;
  invertZoomPrice: boolean;
  invertCursor:    boolean;

  setEnabled: (v: boolean) => void;
  /** Put a command on a slot (replacing what's there); `undefined` empties it. */
  assign: (layer: GamepadLayer, button: number, cmd: GamepadCommandId | undefined) => void;
  /** Move a slot's command onto another slot of the same layer; swaps if occupied. */
  moveSlot: (layer: GamepadLayer, from: number, to: number) => void;
  setDisplay: (cmd: GamepadCommandId, patch: GamepadDisplay | undefined) => void;
  setLayerName: (layer: GamepadModLayer, name: string) => void;
  setAxis: (id: GamepadAxisId, index: number | undefined) => void;
  setShowOverlay: (v: boolean) => void;
  setOverlayOpacity: (v: number) => void;
  setOverlayPos: (p: { x: number; y: number } | null) => void;
  setOverlayScale: (v: number) => void;
  setLeftSensitivity:  (v: number) => void;
  setRightSensitivity: (v: number) => void;
  setInvertZoomTime:  (v: boolean) => void;
  setInvertZoomPrice: (v: boolean) => void;
  setInvertCursor:    (v: boolean) => void;
  resetDefaults: () => void;
}

export const useGamepadStore = create<GamepadState>()(
  persist(
    (set) => ({
      enabled: true,
      layout: cloneLayout(DEFAULT_LAYOUT),
      axes: { ...DEFAULT_AXES },
      display: {},
      layerNames: { ...DEFAULT_LAYER_NAMES },
      showOverlay: true,
      overlayOpacity: 0.9,
      overlayPos: null,
      overlayScale: 1,
      leftSensitivity:  1,
      rightSensitivity: 1,
      invertZoomTime:  false,
      invertZoomPrice: false,
      invertCursor:    false,

      setEnabled: (v) => set({ enabled: v }),
      assign: (layer, button, cmd) => set((s) => {
        if (isModifierButton(button)) return s;
        const slots = { ...s.layout[layer] };
        if (cmd) slots[button] = cmd; else delete slots[button];
        return { layout: { ...s.layout, [layer]: slots } };
      }),
      moveSlot: (layer, from, to) => set((s) => {
        if (from === to || isModifierButton(to)) return s;
        const slots = { ...s.layout[layer] };
        const a = slots[from];
        const b = slots[to];
        if (a) slots[to] = a; else delete slots[to];
        if (b) slots[from] = b; else delete slots[from];
        return { layout: { ...s.layout, [layer]: slots } };
      }),
      setDisplay: (cmd, patch) => set((s) => {
        const display = { ...s.display };
        if (patch) display[cmd] = { ...display[cmd], ...patch }; else delete display[cmd];
        return { display };
      }),
      setLayerName: (layer, name) => set((s) => ({ layerNames: { ...s.layerNames, [layer]: name } })),
      setAxis: (id, index) => set((s) => ({ axes: { ...s.axes, [id]: index } })),
      setShowOverlay: (v) => set({ showOverlay: v }),
      setOverlayOpacity: (v) => set({ overlayOpacity: v }),
      setOverlayPos: (p) => set({ overlayPos: p }),
      setOverlayScale: (v) => set({ overlayScale: clampOverlayScale(v) }),
      setLeftSensitivity:  (v) => set({ leftSensitivity:  v }),
      setRightSensitivity: (v) => set({ rightSensitivity: v }),
      setInvertZoomTime:  (v) => set({ invertZoomTime:  v }),
      setInvertZoomPrice: (v) => set({ invertZoomPrice: v }),
      setInvertCursor:    (v) => set({ invertCursor:    v }),
      resetDefaults: () => set({
        layout: cloneLayout(DEFAULT_LAYOUT),
        axes: { ...DEFAULT_AXES },
        display: {},
        layerNames: { ...DEFAULT_LAYER_NAMES },
      }),
    }),
    {
      name: "tagdash-gamepad",
      storage: userStorage(),
      // v3: back to the W3C "standard" mapping (native Gamepad API) after the gilrs
      // polyfill detour — any older persisted indices are meaningless, so reset.
      // v4: confirm_hod's shipped default (L3, button 10) was an unfinished
      // placeholder that never worked well two-handed with R2 — moved to D-pad →.
      // v5: layers. LB / LT become held modifiers (RT no longer is one); the flat
      // `bindings` map is converted to `layout` + `axes`.
      // v6: replay play / pause, on L3 — where that slot is still free and the
      // command not placed elsewhere (a layout the user arranged is theirs).
      version: 6,
      migrate: (persisted, version) => {
        const { bindings, ...p } = (persisted ?? {}) as Partial<GamepadState> & {
          bindings?: Record<string, LegacyBinding>;
        };
        if (version === 5 && p.layout) {
          const placed = GAMEPAD_LAYERS.some((l) =>
            Object.values(p.layout?.[l] ?? {}).includes("replay_play_pause"));
          if (placed || p.layout.base?.[PAD.L3] !== undefined) return p;
          return { ...p, layout: { ...p.layout, base: { ...p.layout.base, [PAD.L3]: "replay_play_pause" } } };
        }
        if (version >= 5) return p;
        if (version < 3 || !bindings) return p; // no usable layout → defaults (merge)
        const b = { ...bindings };
        // The v4 step, for a blob that skipped it: only replace the old placeholder
        // if the user never rebound it themselves.
        if (version < 4 && b.confirm_hod?.kind === "button" && b.confirm_hod.index === 10) {
          b.confirm_hod = { kind: "button", index: PAD.RIGHT };
        }
        return { ...p, ...migrateLegacy(b) };
      },
      // Fill in whatever the persisted blob predates. The layout itself is taken
      // as-is (an empty slot is a choice, not a missing default).
      merge: (persisted, current) => {
        const p = (persisted ?? {}) as Partial<GamepadState>;
        return {
          ...current,
          ...p,
          layout: p.layout
            ? { base: { ...p.layout.base }, lb: { ...p.layout.lb }, lt: { ...p.layout.lt } }
            : current.layout,
          axes: { ...DEFAULT_AXES, ...(p.axes ?? {}) },
          layerNames: { ...DEFAULT_LAYER_NAMES, ...(p.layerNames ?? {}) },
        };
      },
    },
  ),
);
