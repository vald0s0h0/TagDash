import { create } from "zustand";
import { persist } from "zustand/middleware";
import type { LineStyleName } from "@/stores/chartStore";

// Last-used drawing style → becomes the default for the next drawing of that
// kind. Persisted to localStorage so the user's taste survives restarts. The
// right-click settings panel writes here on every change.

export interface LineStyle {
  color: string;
  opacity: number;       // 0..1
  width: number;         // 1..6
  lineStyle: LineStyleName;
}
export interface TextStyle {
  color: string;
  opacity: number;
  fontSize: number;      // px
}
export interface EmojiStyle {
  fontSize: number;      // px
}

interface DrawingPrefsState {
  line:  LineStyle;
  text:  TextStyle;
  emoji: EmojiStyle;
  setLine:  (p: Partial<LineStyle>) => void;
  setText:  (p: Partial<TextStyle>) => void;
  setEmoji: (p: Partial<EmojiStyle>) => void;
}

const DEFAULT_LINE:  LineStyle  = { color: "#f59e0b", opacity: 1,   width: 2, lineStyle: "solid" };
const DEFAULT_TEXT:  TextStyle  = { color: "#fcd34d", opacity: 1,   fontSize: 12 };
const DEFAULT_EMOJI: EmojiStyle = { fontSize: 24 };

export const useDrawingPrefs = create<DrawingPrefsState>()(
  persist(
    (set) => ({
      line:  DEFAULT_LINE,
      text:  DEFAULT_TEXT,
      emoji: DEFAULT_EMOJI,
      setLine:  (p) => set((s) => ({ line:  { ...s.line,  ...p } })),
      setText:  (p) => set((s) => ({ text:  { ...s.text,  ...p } })),
      setEmoji: (p) => set((s) => ({ emoji: { ...s.emoji, ...p } })),
    }),
    { name: "tagdash-drawing-prefs" },
  ),
);

/** Palette offered by the settings panel (compact, high-contrast on dark). */
export const DRAW_COLORS = [
  "#f59e0b", "#ef4444", "#22c55e", "#3b82f6",
  "#a855f7", "#ec4899", "#eab308", "#e5e7eb",
];
