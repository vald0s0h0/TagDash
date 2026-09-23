import { create } from "zustand";
import { persist } from "zustand/middleware";

// User-tunable chart appearance: candle/volume colours, pre/post-market shading,
// grid, indicators (colour + opacity), execution markers, split markers and the
// SL/TP/alarm price-line colours. Read live by the chart hooks/primitives so an
// edit in Settings → Apparence reflects immediately on every open pane.
//
// Persisted to localStorage (same pattern as the hotkeys / drawing-prefs stores).
//
// IMPORTANT — "my settings = the defaults that ship":
// `DEFAULT_CHART_THEME` below IS the value new installs get. It is seeded with the
// app's current look. To make YOUR tuned palette the shipped default, open
// Settings → Apparence, click "Copier le thème (JSON)", and paste the object over
// `DEFAULT_CHART_THEME` here, then commit. The on-disk (localStorage) value only
// affects the local machine; the constant is what distributions inherit.

// ─── Shape ──────────────────────────────────────────────────────────────────

export interface ChartTheme {
  /** Candle body/border/wick. Volume bars reuse these colours (see `volume`). */
  candle: { up: string; down: string };
  /** Volume histogram opacity (colour taken from the candle up/down colours). */
  volume: { upOpacity: number; downOpacity: number };
  /** Crosshair lines. The vertical line (and its time label) can be switched off
   *  to keep the horizontal price line only. */
  crosshair: { color: string; vertVisible: boolean };
  /** Extended-hours background bands behind intraday candles — one solid zone per
   *  contiguous stretch, with its own colour for pre-market and after-hours. */
  session: {
    preColor: string;  preOpacity: number;
    postColor: string; postOpacity: number;
  };
  /** Thin vertical line at each NY day change (after-hours → next pre-market).
   *  Intraday panes only — a daily bar already spans a whole day. */
  daySeparator: { enabled: boolean; color: string; opacity: number };
  /** Premarket bars on a DAILY pane: the 04:00–09:30 range as a column behind each
   *  regular candle, at 70 % of its width. `fillOpacity` is that column;
   *  `tickOpacity` the hairline BLACK open/close ticks read against it. Same hue
   *  family as `session.preColor` on purpose — premarket reads the same everywhere. */
  premarket: { color: string; fillOpacity: number; tickOpacity: number };
  /** Horizontal price grid lines. */
  grid: { color: string; opacity: number };
  /** Strategy-card indicators. Bollinger is a translucent fill (own opacity). */
  indicators: {
    vwap: string;
    ema: string;
    sma: string;
    bollinger: string;
    bollingerOpacity: number;
  };
  /** Trade-execution markers: buy/sell triangles + the connecting P&L line. */
  executions: { buy: string; sell: string; profit: string; loss: string };
  /** Candle markers: split ex-dates (dot below the bar) + news pastilles (small
   *  dot at the pane bottom, over the volume). */
  markers: { split: string; news: string };
  /** User price lines: stop-loss, take-profit, price alarm, limit order. */
  levels: { sl: string; tp: string; alarm: string; limit: string };
}

/** Top-level theme sections (keys of ChartTheme) — used by the generic setter. */
export type ChartThemeSection = keyof ChartTheme;

// ─── Defaults (shipped values — see file header) ──────────────────────────────

export const DEFAULT_CHART_THEME: ChartTheme = {
  candle: { up: "#26a69a", down: "#ef5350" },
  volume: { upOpacity: 0.4, downOpacity: 0.4 },
  crosshair: { color: "#333333", vertVisible: true },
  session: {
    preColor: "#8296be",  preOpacity: 0.02,
    postColor: "#be9682", postOpacity: 0.02,
  },
  daySeparator: { enabled: true, color: "#ef4444", opacity: 0.55 },
  premarket: { color: "#8296be", fillOpacity: 0.5, tickOpacity: 0.85 },
  grid: { color: "#111111", opacity: 0.5 },
  indicators: {
    vwap: "#3b82f6",
    ema: "#60a5fa",
    sma: "#9ca3af",
    bollinger: "#a78bfa",
    bollingerOpacity: 0.1,
  },
  executions: { buy: "#22c55e", sell: "#ef4444", profit: "#22c55e", loss: "#ef4444" },
  markers: { split: "#ef4444", news: "#38bdf8" },
  levels: { sl: "#ef4444", tp: "#22c55e", alarm: "#f59e0b", limit: "#94a3b8" },
};

// ─── Store ────────────────────────────────────────────────────────────────────

interface ChartThemeState {
  theme: ChartTheme;
  /** Set one field of a section, e.g. set("indicators", "vwap", "#fff"). */
  set: <S extends ChartThemeSection>(
    section: S,
    key: keyof ChartTheme[S],
    value: ChartTheme[S][keyof ChartTheme[S]],
  ) => void;
  /** Restore the shipped defaults. */
  reset: () => void;
}

/** Deep-merge a persisted (possibly older) theme over the current defaults so new
 *  sections/fields added later still get a value. */
function mergeTheme(persisted: Partial<ChartTheme> | undefined): ChartTheme {
  const out = structuredClone(DEFAULT_CHART_THEME) as unknown as Record<string, Record<string, unknown>>;
  if (persisted) {
    const p = persisted as Record<string, unknown>;
    for (const section of Object.keys(out)) {
      const ps = p[section];
      if (ps && typeof ps === "object") {
        // Only keys the current shape defines — a stale key from an older version
        // (e.g. the pre-split `session.color`) must not survive into the theme.
        for (const key of Object.keys(out[section])) {
          const v = (ps as Record<string, unknown>)[key];
          if (v !== undefined) out[section][key] = v;
        }
      }
    }
  }
  // The premarket section changed MEANING when its bars stopped being a full-slot
  // backdrop with a drawn outline and became a 70 %-width column with black
  // open/close ticks: `fillOpacity` was tuned for a sliver peeking past a candle,
  // and `edgeOpacity` no longer exists at all. Carrying that over ships a
  // half-migrated look, so a persisted value from that version resets to defaults.
  // `edgeOpacity` is the tell — only the old shape had it.
  const stalePm = (persisted as { premarket?: Record<string, unknown> } | undefined)?.premarket;
  if (stalePm && stalePm.edgeOpacity !== undefined) {
    out.premarket = { ...DEFAULT_CHART_THEME.premarket } as unknown as Record<string, unknown>;
  }

  // Legacy (single-tint) session shading: `{ color, opacity }` predates the split
  // into pre-market / after-hours. Carry the old tint over to both so an existing
  // install keeps the look it had until the user picks two colours.
  const legacy = (persisted as { session?: { color?: string; opacity?: number } } | undefined)?.session;
  const sess = out.session as { preColor: string; preOpacity: number; postColor: string; postOpacity: number };
  if (legacy?.color && !(persisted?.session as Record<string, unknown> | undefined)?.preColor) {
    sess.preColor = legacy.color;
    sess.postColor = legacy.color;
  }
  if (typeof legacy?.opacity === "number" && (persisted?.session as Record<string, unknown> | undefined)?.preOpacity == null) {
    sess.preOpacity = legacy.opacity;
    sess.postOpacity = legacy.opacity;
  }
  return out as unknown as ChartTheme;
}

export const useChartThemeStore = create<ChartThemeState>()(
  persist(
    (set) => ({
      theme: structuredClone(DEFAULT_CHART_THEME),
      set: (section, key, value) =>
        set((s) => ({
          theme: { ...s.theme, [section]: { ...s.theme[section], [key]: value } },
        })),
      reset: () => set({ theme: structuredClone(DEFAULT_CHART_THEME) }),
    }),
    {
      name: "tagdash-chart-theme",
      // Persist only the palette; merge forward over the latest defaults.
      partialize: (s) => ({ theme: s.theme }),
      merge: (persisted, current) => ({
        ...current,
        theme: mergeTheme((persisted as { theme?: Partial<ChartTheme> } | undefined)?.theme),
      }),
    },
  ),
);

// ─── Accessors ────────────────────────────────────────────────────────────────

/** React hook — subscribes to the live theme (re-renders on any edit). */
export function useChartTheme(): ChartTheme {
  return useChartThemeStore((s) => s.theme);
}

/** Non-React snapshot — for chart primitives that read at draw time. */
export function getChartTheme(): ChartTheme {
  return useChartThemeStore.getState().theme;
}
