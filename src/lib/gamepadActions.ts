import {
  GAMEPAD_ACTIONS, type GamepadActionId, type GamepadBinding,
} from "@/stores/gamepadStore";

// Pure (React-free) helpers shared by the gamepad loop. Stateful dispatch
// (navigation, zone calls, query data) lives in useGamepad where the React data
// is available; this file is just the binding/axis math so it stays testable.

type Bindings = Record<GamepadActionId, GamepadBinding>;

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

/** A trigger/button treated as a held modifier (R2): pressed or analog > 0.5. */
export function buttonHeld(pad: Gamepad, b: GamepadBinding | undefined): boolean {
  if (!b || b.kind !== "button") return false;
  const btn = pad.buttons[b.index];
  return !!btn && (btn.pressed || btn.value > 0.5);
}

/** Is `id` bound to button `idx`? */
export function isBoundToButton(bindings: Bindings, id: GamepadActionId, idx: number): boolean {
  const b = bindings[id];
  return b?.kind === "button" && b.index === idx;
}

/** Resolve the command a just-pressed button drives, honouring the R2 layer: when
 *  R2 is held an armed-layer command on that button wins over its cursor-layer
 *  twin; otherwise the first non-armed, non-reserved command bound to it fires.
 *  (Navigation buttons have no armed twin, so they keep working under R2.) */
export function resolveAction(
  bindings: Bindings, idx: number, r2Held: boolean,
): GamepadActionId | null {
  if (r2Held) {
    for (const a of GAMEPAD_ACTIONS) {
      if (a.kind === "digital" && a.armed && isBoundToButton(bindings, a.id, idx)) return a.id;
    }
  }
  for (const a of GAMEPAD_ACTIONS) {
    if (a.kind === "digital" && !a.armed && !a.reserved && isBoundToButton(bindings, a.id, idx)) return a.id;
  }
  return null;
}
