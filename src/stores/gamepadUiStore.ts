import { create } from "zustand";
import type { GamepadLayer } from "@/stores/gamepadStore";

// Transient controller UI state driven by the gamepad loop (RAM only):
//  • the auto-tag picker (Share button) + a press counter the modal watches,
//  • a flash error toast (e.g. Share with no active trade),
//  • the live pad state the on-screen overlay + Settings preview draw.

/** Live pad state. Written by the loop only when something changes. */
export interface GamepadPadState {
  /** A controller is enumerated. */
  connected: boolean;
  /** …and the loop is actually dispatching (enabled, trading view, no dialog). */
  active: boolean;
  /** Set of buttons currently in effect (LB / LT held). */
  layer: GamepadLayer;
  /** Pressed buttons, bit i = button i. */
  pressed: number;
}

interface GamepadUiState extends GamepadPadState {
  /** Non-null ⇒ the auto-tag picker is open for this trade. */
  tag: { tradeId: string; symbol: string } | null;
  /** Incremented on each Share press while the picker is open (advances the
   *  highlight + resets the 2 s confirm timer, both owned by the modal). */
  tagAdvance: number;
  /** Short-lived error message shown as a flash toast (null = hidden). */
  flashError: string | null;

  openTagPicker:  (tradeId: string, symbol: string) => void;
  bumpTagAdvance: () => void;
  closeTagPicker: () => void;
  setFlashError:  (msg: string | null) => void;
  setPad:         (p: GamepadPadState) => void;
}

export const useGamepadUiStore = create<GamepadUiState>((set) => ({
  tag: null,
  tagAdvance: 0,
  flashError: null,
  connected: false,
  active: false,
  layer: "base",
  pressed: 0,

  openTagPicker:  (tradeId, symbol) => set({ tag: { tradeId, symbol }, tagAdvance: 0 }),
  bumpTagAdvance: () => set((s) => ({ tagAdvance: s.tagAdvance + 1 })),
  closeTagPicker: () => set({ tag: null }),
  setFlashError:  (msg) => set({ flashError: msg }),
  setPad:         (p) => set(p),
}));
