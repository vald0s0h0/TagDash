import { create } from "zustand";
import { persist } from "zustand/middleware";
import type { Timeframe } from "@/types";

// User-configurable hotkeys (keyboard chords or extra mouse buttons) bound to the
// per-zone chart actions. A hotkey is global but acts on the zone under the mouse
// cursor (its left/interactive pane), falling back to the active tab's first zone.
// Bindings persist to localStorage (same pattern as the drawing prefs); the
// command dispatch itself lives in each mounted ChartZone, registered here by
// zone_id so the global listener can route an action to the right zone.

// ─── Bindable actions ─────────────────────────────────────────────────────────

export type HotkeyActionId =
  | "release" | "sl" | "tp" | "alarm" | "line" | "text"
  | "capture" | "journal"
  | "order_mode" | "order_25" | "order_50" | "order_100" | "close"
  | "confirm_hod"
  | "run_llm"
  | "tf_5s" | "tf_10s" | "tf_1m" | "tf_2m" | "tf_5m" | "tf_15m" | "tf_daily"
  // Replay (global — not tied to a chart zone, special-cased in useHotkeys)
  | "replay_next_alert";

export type HotkeyGroup = "Toolbar" | "Ordres" | "Analyse" | "Timeframes" | "Replay";

export interface HotkeyActionDef {
  id: HotkeyActionId;
  label: string;
  group: HotkeyGroup;
}

export const HOTKEY_ACTIONS: HotkeyActionDef[] = [
  { id: "sl",        label: "Mode Stop Loss",          group: "Toolbar" },
  { id: "tp",        label: "Mode Take Profit",        group: "Toolbar" },
  { id: "alarm",     label: "Mode alarme",             group: "Toolbar" },
  { id: "line",      label: "Mode ligne",              group: "Toolbar" },
  { id: "text",      label: "Mode texte",              group: "Toolbar" },
  { id: "capture",   label: "Capture d'écran",         group: "Toolbar" },
  { id: "journal",   label: "Ouvrir le journal",       group: "Toolbar" },
  { id: "release",   label: "Libérer la zone",         group: "Toolbar" },

  { id: "order_mode", label: "Basculer Market / Limit", group: "Ordres" },
  { id: "order_25",   label: "Ordre 25 %",              group: "Ordres" },
  { id: "order_50",   label: "Ordre 50 %",              group: "Ordres" },
  { id: "order_100",  label: "Ordre 100 %",             group: "Ordres" },
  { id: "close",       label: "Clôturer la position",    group: "Ordres" },
  { id: "confirm_hod", label: "Confirmer HOD Drive",     group: "Ordres" },

  { id: "run_llm",    label: "Lancer l'analyse IA",     group: "Analyse" },

  { id: "tf_5s",    label: "Timeframe 5s",    group: "Timeframes" },
  { id: "tf_10s",   label: "Timeframe 10s",   group: "Timeframes" },
  { id: "tf_1m",    label: "Timeframe 1m",    group: "Timeframes" },
  { id: "tf_2m",    label: "Timeframe 2m",    group: "Timeframes" },
  { id: "tf_5m",    label: "Timeframe 5m",    group: "Timeframes" },
  { id: "tf_15m",   label: "Timeframe 15m",   group: "Timeframes" },
  { id: "tf_daily", label: "Timeframe daily", group: "Timeframes" },

  { id: "replay_next_alert", label: "Replay : prochaine alerte", group: "Replay" },
];

export const HOTKEY_GROUPS: HotkeyGroup[] = ["Toolbar", "Ordres", "Analyse", "Timeframes", "Replay"];

/** Map a timeframe action to its Timeframe value (null for non-timeframe actions). */
export const TF_FOR_ACTION: Partial<Record<HotkeyActionId, Timeframe>> = {
  tf_5s: "5s", tf_10s: "10s", tf_1m: "1m", tf_2m: "2m",
  tf_5m: "5m", tf_15m: "15m", tf_daily: "daily",
};

// ─── Binding model ────────────────────────────────────────────────────────────

export interface Binding {
  /** "key" → KeyboardEvent.code; "mouse" → MouseEvent.button (as string). */
  kind: "key" | "mouse";
  code: string;
  ctrl:  boolean;
  alt:   boolean;
  shift: boolean;
  meta:  boolean;
}

export function bindingsEqual(a: Binding, b: Binding): boolean {
  return a.kind === b.kind && a.code === b.code
    && a.ctrl === b.ctrl && a.alt === b.alt && a.shift === b.shift && a.meta === b.meta;
}

const MODIFIER_CODES = new Set([
  "ControlLeft", "ControlRight", "AltLeft", "AltRight",
  "ShiftLeft", "ShiftRight", "MetaLeft", "MetaRight",
]);

/** Build a Binding from a DOM event, or null when the event isn't bindable
 *  (a lone modifier key, or a reserved mouse button: left = click, right =
 *  context menu). Modifiers (ctrl/alt/shift/meta) are folded into the chord. */
export function bindingFromEvent(e: KeyboardEvent | MouseEvent): Binding | null {
  const mods = { ctrl: e.ctrlKey, alt: e.altKey, shift: e.shiftKey, meta: e.metaKey };
  if ("button" in e) {
    // Left (0) and right (2) buttons are reserved for click and context menu.
    if (e.button === 0 || e.button === 2) return null;
    return { kind: "mouse", code: String(e.button), ...mods };
  }
  const code = e.code;
  if (!code || MODIFIER_CODES.has(code)) return null;
  return { kind: "key", code, ...mods };
}

function keyLabel(code: string): string {
  if (code.startsWith("Key"))    return code.slice(3);
  if (code.startsWith("Digit"))  return code.slice(5);
  if (code.startsWith("Numpad")) return "Num " + code.slice(6);
  const map: Record<string, string> = {
    Space: "Espace", Enter: "Entrée", Escape: "Échap", Tab: "Tab",
    ArrowUp: "↑", ArrowDown: "↓", ArrowLeft: "←", ArrowRight: "→",
    Backquote: "`", Minus: "-", Equal: "=", BracketLeft: "[", BracketRight: "]",
    Semicolon: ";", Quote: "'", Comma: ",", Period: ".", Slash: "/", Backslash: "\\",
  };
  return map[code] ?? code;
}

function mouseLabel(code: string): string {
  const n = Number(code);
  if (n === 1) return "Clic molette";
  // 3 → "Souris 4" (back), 4 → "Souris 5" (forward), etc.
  return `Souris ${n + 1}`;
}

/** Human-readable label for a binding, e.g. "Ctrl + Shift + T" / "Souris 4". */
export function bindingLabel(b: Binding): string {
  const parts: string[] = [];
  if (b.ctrl)  parts.push("Ctrl");
  if (b.alt)   parts.push("Alt");
  if (b.shift) parts.push("Shift");
  if (b.meta)  parts.push("Meta");
  parts.push(b.kind === "mouse" ? mouseLabel(b.code) : keyLabel(b.code));
  return parts.join(" + ");
}

// ─── Persisted bindings store ─────────────────────────────────────────────────

interface HotkeyState {
  bindings: Partial<Record<HotkeyActionId, Binding>>;
  setBinding:   (id: HotkeyActionId, b: Binding) => void;
  clearBinding: (id: HotkeyActionId) => void;
}

export const useHotkeyStore = create<HotkeyState>()(
  persist(
    (set) => ({
      bindings: {},
      setBinding: (id, b) =>
        set((s) => {
          // A chord is unique: strip it off any other action it was bound to.
          const next = { ...s.bindings };
          for (const k of Object.keys(next) as HotkeyActionId[]) {
            const cur = next[k];
            if (cur && bindingsEqual(cur, b)) delete next[k];
          }
          next[id] = b;
          return { bindings: next };
        }),
      clearBinding: (id) =>
        set((s) => {
          const next = { ...s.bindings };
          delete next[id];
          return { bindings: next };
        }),
    }),
    { name: "tagdash-hotkeys" },
  ),
);

// ─── Per-zone dispatch registry + hover tracking (module state, no re-renders) ──

type Dispatcher = (id: HotkeyActionId) => void;
const zoneDispatchers = new Map<string, Dispatcher>();
let hoveredZoneId: string | null = null;
let recordingActive = false;

/** A mounted ChartZone registers its action runner; returns an unregister fn. */
export function registerZoneHotkeys(zoneId: string, fn: Dispatcher): () => void {
  zoneDispatchers.set(zoneId, fn);
  return () => { if (zoneDispatchers.get(zoneId) === fn) zoneDispatchers.delete(zoneId); };
}

export function dispatchToZone(zoneId: string, id: HotkeyActionId): void {
  zoneDispatchers.get(zoneId)?.(id);
}

export function setHoveredZone(zoneId: string | null): void { hoveredZoneId = zoneId; }
export function getHoveredZone(): string | null { return hoveredZoneId; }

/** Set while the Settings recorder is capturing a chord, so the global listener
 *  doesn't fire an action for the very key/button being assigned. */
export function setRecordingActive(v: boolean): void { recordingActive = v; }
export function isRecordingActive(): boolean { return recordingActive; }
