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
  /** Extended-hours (pre/post-market) background tint behind intraday candles. */
  session: { color: string; opacity: number };
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
  /** User price lines: stop-loss, take-profit, price alarm. */
  levels: { sl: string; tp: string; alarm: string };
}

/** Top-level theme sections (keys of ChartTheme) — used by the generic setter. */
export type ChartThemeSection = keyof ChartTheme;

// ─── Defaults (shipped values — see file header) ──────────────────────────────

export const DEFAULT_CHART_THEME: ChartTheme = {
  candle: { up: "#26a69a", down: "#ef5350" },
  volume: { upOpacity: 0.4, downOpacity: 0.4 },
  session: { color: "#8296be", opacity: 0.02 },
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
  levels: { sl: "#ef4444", tp: "#22c55e", alarm: "#f59e0b" },
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
        out[section] = { ...out[section], ...(ps as Record<string, unknown>) };
      }
    }
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
