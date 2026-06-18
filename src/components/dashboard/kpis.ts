// Pure KPI / series math over the mirrored TradeTally trades. Kept framework-free
// so new cards can derive new metrics without touching the backend.

import type { DashboardTrade } from "@/types";

function round2(v: number): number {
  return Math.round(v * 100) / 100;
}

function dateKey(t: DashboardTrade): string {
  return t.exit_date ?? t.entry_date ?? "";
}

/** Closed trades with a realized P&L, oldest first. A trade counts as closed when
 *  its status says so, or (defensively) whenever it carries a non-null pnl. */
export function closedTrades(trades: DashboardTrade[]): DashboardTrade[] {
  return trades
    .filter((t) => t.pnl != null && (t.status == null || t.status.toLowerCase() === "closed"))
    .slice()
    .sort((a, b) => dateKey(a).localeCompare(dateKey(b)));
}

/** Profit factor = gross profit / gross loss. Infinity when there are wins but no
 *  losses; null when there are no trades. */
export function profitFactor(trades: DashboardTrade[]): number | null {
  let gross = 0;
  let loss = 0;
  for (const t of trades) {
    const p = t.pnl ?? 0;
    if (p > 0) gross += p;
    else if (p < 0) loss += -p;
  }
  if (trades.length === 0) return null;
  if (loss === 0) return gross > 0 ? Infinity : null;
  return gross / loss;
}

export interface PnlPoint {
  i: number;
  date: string;
  cumulative: number;
  pnl: number;
}

/** Cumulative realized P&L (equity curve) over the closed trades. */
export function pnlCurve(trades: DashboardTrade[]): PnlPoint[] {
  let cum = 0;
  return trades.map((t, i) => {
    cum += t.pnl ?? 0;
    return { i, date: dateKey(t), cumulative: round2(cum), pnl: round2(t.pnl ?? 0) };
  });
}

export interface RpfPoint {
  i: number;
  date: string;
  pf: number;
}

/** Rolling profit factor over a trailing window (default 20 trades). A window with
 *  no losses is clamped to `cap` so the line stays visible (rather than spiking to
 *  infinity). */
export function rollingProfitFactor(
  trades: DashboardTrade[],
  window = 20,
  cap = 5
): RpfPoint[] {
  return trades.map((t, i) => {
    const slice = trades.slice(Math.max(0, i - window + 1), i + 1);
    const pf = profitFactor(slice);
    let v: number;
    if (pf == null) v = 0;
    else if (!isFinite(pf)) v = cap;
    else v = round2(Math.min(pf, cap * 4)); // soft outlier guard
    return { i, date: dateKey(t), pf: v };
  });
}

export interface KpiSummary {
  profitFactor: number | null;
  winRate: number; // 0..1
  totalPnl: number;
  count: number;
  avgWin: number;
  avgLoss: number; // positive magnitude
  expectancy: number; // average P&L per trade
  largestWin: number;
  largestLoss: number; // negative
}

export function summarize(trades: DashboardTrade[]): KpiSummary {
  const pnls = trades.map((t) => t.pnl ?? 0);
  const wins = pnls.filter((p) => p > 0);
  const losses = pnls.filter((p) => p < 0);
  const count = trades.length;
  const totalPnl = pnls.reduce((a, b) => a + b, 0);
  const avgWin = wins.length ? wins.reduce((a, b) => a + b, 0) / wins.length : 0;
  const avgLoss = losses.length ? -losses.reduce((a, b) => a + b, 0) / losses.length : 0;
  return {
    profitFactor: profitFactor(trades),
    winRate: count ? wins.length / count : 0,
    totalPnl: round2(totalPnl),
    count,
    avgWin: round2(avgWin),
    avgLoss: round2(avgLoss),
    expectancy: count ? round2(totalPnl / count) : 0,
    largestWin: round2(pnls.length ? Math.max(...pnls, 0) : 0),
    largestLoss: round2(pnls.length ? Math.min(...pnls, 0) : 0),
  };
}

// ─── Display formatters ────────────────────────────────────────────────────────

export function formatMoney(v: number): string {
  const sign = v < 0 ? "-" : "";
  return `${sign}$${Math.abs(v).toLocaleString(undefined, { maximumFractionDigits: 0 })}`;
}

export function formatPf(v: number | null): string {
  if (v == null) return "—";
  if (!isFinite(v)) return "∞";
  return v.toFixed(2);
}

export function formatPct(v: number): string {
  return `${(v * 100).toFixed(0)}%`;
}
