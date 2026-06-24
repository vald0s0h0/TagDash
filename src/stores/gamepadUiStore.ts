import { create } from "zustand";

// Transient controller UI state driven by the gamepad loop (RAM only):
//  • the auto-tag picker (Share button) + a press counter the modal watches,
//  • a flash error toast (e.g. Share with no active trade),
//  • the R2 "armed layer" overlay visibility.

interface GamepadUiState {
  /** Non-null ⇒ the auto-tag picker is open for this trade. */
  tag: { tradeId: string; symbol: string } | null;
  /** Incremented on each Share press while the picker is open (advances the
   *  highlight + resets the 2 s confirm timer, both owned by the modal). */
  tagAdvance: number;
  /** Short-lived error message shown as a flash toast (null = hidden). */
  flashError: string | null;
  /** True while R2 is held (shows the armed-layer overlay). */
  armed: boolean;

  openTagPicker:  (tradeId: string, symbol: string) => void;
  bumpTagAdvance: () => void;
  closeTagPicker: () => void;
  setFlashError:  (msg: string | null) => void;
  setArmed:       (v: boolean) => void;
}

export const useGamepadUiStore = create<GamepadUiState>((set) => ({
  tag: null,
  tagAdvance: 0,
  flashError: null,
  armed: false,

  openTagPicker:  (tradeId, symbol) => set({ tag: { tradeId, symbol }, tagAdvance: 0 }),
  bumpTagAdvance: () => set((s) => ({ tagAdvance: s.tagAdvance + 1 })),
  closeTagPicker: () => set({ tag: null }),
  setFlashError:  (msg) => set({ flashError: msg }),
  setArmed:       (v) => set({ armed: v }),
}));
