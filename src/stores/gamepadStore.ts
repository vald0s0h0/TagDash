import { create } from "zustand";
import { persist } from "zustand/middleware";

// Xbox-controller bindings + tuning, persisted to localStorage (same pattern as
// hotkeyStore). The whole point of persisting is that the maintainer's chosen
// layout/sensitivities become the bundled distribution defaults (DEFAULT_BINDINGS
// below = the layout described in the spec). The controller is auto-active the
// moment the OS enumerates it (Web Gamepad API over USB-C or Bluetooth) — the
// `enabled` switch is just an opt-out.
//
// Two command kinds, matching the two parts of the spec:
//   • digital — a single button press (edge-triggered, like a keyboard chord).
//   • analog  — a stick AXIS returning a continuous −1..+1 value (zoom / cursor).
// A digital command can only bind to a button, an analog command only to an axis;
// the Settings recorder rejects the mismatch with an explanatory message.

// ─── Bindable commands ────────────────────────────────────────────────────────

export type GamepadActionId =
  // Navigation (digital)
  | "ticker_prev" | "ticker_next" | "ticker_release"
  | "tab_prev" | "tab_next" | "focus_cycle"
  // Cursor / order layer — R2 NOT held (digital)
  | "cursor_sl" | "cursor_alarm" | "cursor_tp" | "remove_orders"
  // Armed layer — R2 held (digital)
  | "r2_modifier" | "order_25" | "order_50" | "order_100" | "close_position"
  // Journal / TradeTally (digital)
  | "capture" | "share_tag" | "journal_audio"
  // Analog axes
  | "zoom_time" | "zoom_price" | "cursor_move";

export type GamepadActionKind = "digital" | "analog";

export type GamepadGroup =
  | "Navigation" | "Curseur / Ordres" | "Position armée"
  | "Journal / TradeTally" | "Axes (analogique)";

export interface GamepadActionDef {
  id: GamepadActionId;
  label: string;
  group: GamepadGroup;
  kind: GamepadActionKind;
  /** Belongs to the R2-held armed layer (its binding may share a button with a
   *  cursor-layer command — the modifier disambiguates at dispatch time). */
  armed?: boolean;
  /** Reserved for a future feature (journal audio / Whisper): bindable + shown,
   *  but the dispatch ignores it for now. */
  reserved?: boolean;
}

export const GAMEPAD_ACTIONS: GamepadActionDef[] = [
  // Navigation
  { id: "ticker_prev",    label: "Ticker précédent",         group: "Navigation", kind: "digital" },
  { id: "ticker_next",    label: "Ticker suivant",           group: "Navigation", kind: "digital" },
  { id: "ticker_release", label: "Libérer / supprimer",      group: "Navigation", kind: "digital" },
  { id: "tab_prev",       label: "Onglet précédent",         group: "Navigation", kind: "digital" },
  { id: "tab_next",       label: "Onglet suivant",           group: "Navigation", kind: "digital" },
  { id: "focus_cycle",    label: "Changer le focus du chart", group: "Navigation", kind: "digital" },

  // Cursor / order layer (R2 not held)
  { id: "cursor_sl",     label: "Stop Loss au curseur",   group: "Curseur / Ordres", kind: "digital" },
  { id: "cursor_alarm",  label: "Alarme au curseur",      group: "Curseur / Ordres", kind: "digital" },
  { id: "cursor_tp",     label: "Take Profit au curseur", group: "Curseur / Ordres", kind: "digital" },
  { id: "remove_orders", label: "Retirer les ordres (double = + alarmes)", group: "Curseur / Ordres", kind: "digital" },

  // Armed layer (R2 held)
  { id: "r2_modifier",   label: "Maintenir = position armée", group: "Position armée", kind: "digital" },
  { id: "order_25",      label: "Position 25 %",  group: "Position armée", kind: "digital", armed: true },
  { id: "order_50",      label: "Position 50 %",  group: "Position armée", kind: "digital", armed: true },
  { id: "order_100",     label: "Position 100 %", group: "Position armée", kind: "digital", armed: true },
  { id: "close_position", label: "Clôturer la position", group: "Position armée", kind: "digital", armed: true },

  // Journal / TradeTally
  { id: "capture",       label: "Capture TradeTally",          group: "Journal / TradeTally", kind: "digital" },
  { id: "share_tag",     label: "Ajouter un tag (TradeTally)", group: "Journal / TradeTally", kind: "digital" },
  { id: "journal_audio", label: "Note audio (dictée trade)",   group: "Journal / TradeTally", kind: "digital" },

  // Analog axes
  { id: "zoom_time",   label: "Zoom horizontal (temps)", group: "Axes (analogique)", kind: "analog" },
  { id: "zoom_price",  label: "Zoom vertical (prix)",    group: "Axes (analogique)", kind: "analog" },
  { id: "cursor_move", label: "Curseur horizontal",      group: "Axes (analogique)", kind: "analog" },
];

export const GAMEPAD_GROUPS: GamepadGroup[] = [
  "Navigation", "Curseur / Ordres", "Position armée", "Journal / TradeTally", "Axes (analogique)",
];

export function actionDef(id: GamepadActionId): GamepadActionDef | undefined {
  return GAMEPAD_ACTIONS.find((a) => a.id === id);
}

// ─── Binding model ────────────────────────────────────────────────────────────

export type GamepadBinding =
  | { kind: "button"; index: number }
  | { kind: "axis"; index: number };

export function bindingsEqual(a: GamepadBinding, b: GamepadBinding): boolean {
  return a.kind === b.kind && a.index === b.index;
}

// Index → label for the W3C "standard" gamepad mapping (the webview's native
// Gamepad API). The recorder writes raw indices, so an unusual pad/OS just shows
// the fallback ("Bouton N" / "Axe N") until rebound.
const BUTTON_LABELS: Record<number, string> = {
  0: "A", 1: "B", 2: "X", 3: "Y",
  4: "LB (L1)", 5: "RB (R1)", 6: "LT (L2)", 7: "RT (R2)",
  8: "View", 9: "Menu", 10: "L3", 11: "R3",
  12: "D-pad ↑", 13: "D-pad ↓", 14: "D-pad ←", 15: "D-pad →",
  16: "Guide", 17: "Share",
};
const AXIS_LABELS: Record<number, string> = {
  0: "Stick gauche ↔", 1: "Stick gauche ↕",
  2: "Stick droit ↔",  3: "Stick droit ↕",
};

export function bindingLabel(b: GamepadBinding | undefined): string {
  if (!b) return "non assigné";
  return b.kind === "button"
    ? (BUTTON_LABELS[b.index] ?? `Bouton ${b.index}`)
    : (AXIS_LABELS[b.index] ?? `Axe ${b.index}`);
}

// ─── Default layout (the spec; = distribution defaults) ───────────────────────
// W3C "standard" mapping (native Gamepad API): A=0 B=1 X=2 Y=3 · LB=4 RB=5 LT=6
// RT=7 · View=8 Menu=9 L3=10 R3=11 · D-pad 12↑/13↓/14←/15→ · Guide=16 Share=17 ·
// axes LX=0 LY=1 RX=2 RY=3. Auto-tag on D-pad → (Xbox One/360 have no Share).
// PS names map to Xbox: L1=LB L2=LT R1=RB R2=RT.

export const DEFAULT_BINDINGS: Record<GamepadActionId, GamepadBinding> = {
  // Navigation
  ticker_prev:    { kind: "button", index: 12 }, // D-pad ↑
  ticker_next:    { kind: "button", index: 13 }, // D-pad ↓
  ticker_release: { kind: "button", index: 14 }, // D-pad ←
  tab_prev:       { kind: "button", index: 4 },  // L1 / LB
  tab_next:       { kind: "button", index: 6 },  // L2 / LT
  focus_cycle:    { kind: "button", index: 5 },  // R1 / RB

  // Cursor / order layer (R2 up)
  cursor_sl:     { kind: "button", index: 0 }, // A
  cursor_alarm:  { kind: "button", index: 1 }, // B
  cursor_tp:     { kind: "button", index: 3 }, // Y
  remove_orders: { kind: "button", index: 2 }, // X

  // Armed layer (R2 held) — share the face buttons
  r2_modifier:    { kind: "button", index: 7 }, // R2 / RT
  order_25:       { kind: "button", index: 0 }, // A
  order_50:       { kind: "button", index: 1 }, // B
  order_100:      { kind: "button", index: 3 }, // Y
  close_position: { kind: "button", index: 2 }, // X

  // Journal / TradeTally
  capture:       { kind: "button", index: 8 },  // View / Back
  share_tag:     { kind: "button", index: 15 }, // D-pad → (no Share on this pad)
  journal_audio: { kind: "button", index: 9 },  // Menu / Start (reserved no-op)

  // Analog axes
  zoom_time:   { kind: "axis", index: 0 }, // left stick X
  zoom_price:  { kind: "axis", index: 1 }, // left stick Y
  cursor_move: { kind: "axis", index: 3 }, // right stick Y
};

// ─── Persisted store ──────────────────────────────────────────────────────────

interface GamepadState {
  enabled: boolean;
  bindings: Record<GamepadActionId, GamepadBinding>;
  /** Stick speed multipliers (0.2 = slow … 3 = fast). */
  leftSensitivity:  number; // zoom (left stick)
  rightSensitivity: number; // cursor (right stick)
  invertZoomTime:  boolean;
  invertZoomPrice: boolean;
  invertCursor:    boolean;

  setEnabled:      (v: boolean) => void;
  setBinding:      (id: GamepadActionId, b: GamepadBinding) => void;
  setLeftSensitivity:  (v: number) => void;
  setRightSensitivity: (v: number) => void;
  setInvertZoomTime:  (v: boolean) => void;
  setInvertZoomPrice: (v: boolean) => void;
  setInvertCursor:    (v: boolean) => void;
  resetDefaults:   () => void;
}

export const useGamepadStore = create<GamepadState>()(
  persist(
    (set) => ({
      enabled: true,
      bindings: { ...DEFAULT_BINDINGS },
      leftSensitivity:  1,
      rightSensitivity: 1,
      invertZoomTime:  false,
      invertZoomPrice: false,
      invertCursor:    false,

      setEnabled: (v) => set({ enabled: v }),
      setBinding: (id, b) => set((s) => ({ bindings: { ...s.bindings, [id]: b } })),
      setLeftSensitivity:  (v) => set({ leftSensitivity:  v }),
      setRightSensitivity: (v) => set({ rightSensitivity: v }),
      setInvertZoomTime:  (v) => set({ invertZoomTime:  v }),
      setInvertZoomPrice: (v) => set({ invertZoomPrice: v }),
      setInvertCursor:    (v) => set({ invertCursor:    v }),
      resetDefaults: () => set({ bindings: { ...DEFAULT_BINDINGS } }),
    }),
    {
      name: "tagdash-gamepad",
      // v3: back to the W3C "standard" mapping (native Gamepad API) after the gilrs
      // polyfill detour — any older persisted indices are meaningless, so reset.
      version: 3,
      migrate: (persisted, version) => {
        const p = (persisted ?? {}) as Partial<GamepadState>;
        if (version < 3) return { ...p, bindings: { ...DEFAULT_BINDINGS } };
        return p;
      },
      // Fill in any newly-added default bindings the persisted blob predates, so
      // adding a command later doesn't leave it unbound.
      merge: (persisted, current) => {
        const p = (persisted ?? {}) as Partial<GamepadState>;
        return {
          ...current,
          ...p,
          bindings: { ...DEFAULT_BINDINGS, ...(p.bindings ?? {}) },
        };
      },
    },
  ),
);
