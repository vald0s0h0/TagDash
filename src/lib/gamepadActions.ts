import {
  LAYER_MODIFIER,
  type GamepadCommandId, type GamepadLayer, type GamepadLayout,
} from "@/stores/gamepadStore";

// Pure (React-free) helpers shared by the gamepad loop. Stateful dispatch
// (navigation, zone calls, query data) lives in useGamepad where the React data
// is available; this file is just the layer/axis math so it stays testable.

/** Choose the actual game controller among ALL devices navigator.getGamepads()
 *  exposes. Other HID gear (3Dconnexion SpaceMouse, flight gear, …) also shows up
 *  there but isn't a gamepad — picking pads[0] grabs whichever enumerated first.
 *  Prefer a W3C "standard"-mapped pad (what Xbox/PlayStation controllers report),
 *  else the connected device with the most buttons (a real pad has ~17; a Space-
 *  Mouse has 2). */
export function pickGamepad(pads: (Gamepad | null)[]): Gamepad | null {
  const connected = [...pads].filter((p): p is Gamepad => !!p && p.connected);
  if (connected.length === 0) return null;
  return connected.find((p) => p.mapping === "standard")
    ?? connected.reduce((best, p) => (p.buttons.length > best.buttons.length ? p : best));
}

/** Stick deadzone — ignore resting drift. */
export const DEADZONE = 0.18;

/** Map a raw axis value (−1..1) through the deadzone and an accelerating response
 *  (squared past the deadzone, so it's slow for fine control and fast near the
 *  extremes — the spec's "plus c'est proche de ±1, plus c'est rapide"), scaled by
 *  the user's sensitivity. Returns a signed speed (~−s..+s), 0 inside the deadzone. */
export function axisSpeed(raw: number, sensitivity: number): number {
  const a = Math.abs(raw);
  if (a < DEADZONE) return 0;
  const t = (a - DEADZONE) / (1 - DEADZONE); // re-normalise past the deadzone
  return Math.sign(raw) * t * t * sensitivity;
}

/** A bumper/trigger treated as a held modifier: pressed or analog > 0.5. */
export function buttonHeld(pad: Gamepad, index: number): boolean {
  const btn = pad.buttons[index];
  return !!btn && (btn.pressed || btn.value > 0.5);
}

/** The layer the pad is on. The modifier held FIRST keeps its layer until it's
 *  released (pressing the other one on top changes nothing), so a set of buttons
 *  never swaps under the thumb mid-gesture. */
export function heldLayer(pad: Gamepad, prev: GamepadLayer): GamepadLayer {
  if (prev !== "base" && buttonHeld(pad, LAYER_MODIFIER[prev])) return prev;
  if (buttonHeld(pad, LAYER_MODIFIER.lt)) return "lt";
  if (buttonHeld(pad, LAYER_MODIFIER.lb)) return "lb";
  return "base";
}

/** The command a just-pressed button drives on `layer`. Layers are strict: an
 *  empty slot fires nothing — there is no fall-through to the base set. */
export function resolveAction(
  layout: GamepadLayout, layer: GamepadLayer, idx: number,
): GamepadCommandId | null {
  return layout[layer]?.[idx] ?? null;
}

/** Pressed buttons as a bitmask (bit i = button i) — one cheap value the UI can
 *  subscribe to for press feedback. */
export function pressedMask(pad: Gamepad): number {
  let mask = 0;
  const n = Math.min(pad.buttons.length, 31);
  for (let i = 0; i < n; i++) if (pad.buttons[i].pressed) mask |= 1 << i;
  return mask;
}
