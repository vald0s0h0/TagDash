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

/** Rolling profit factor over a trailing window of exactly `window` trades
 *  (default 20). Only emits a point once the window has actually started
 *  rolling — i.e. once there are more than `window` closed trades — so a
 *  partial or single static first-window value never gets plotted. With 121
 *  trades and window=100, this yields 21 points (indices 100..120). A window
 *  with no losses is clamped to `cap` so the line stays visible (rather than
 *  spiking to infinity). */
export function rollingProfitFactor(
  trades: DashboardTrade[],
  window = 20,
  cap = 5
): RpfPoint[] {
  const result: RpfPoint[] = [];
  for (let i = window; i < trades.length; i++) {
    const slice = trades.slice(i - window + 1, i + 1);
    const pf = profitFactor(slice);
    let v: number;
    if (pf == null) v = 0;
    else if (!isFinite(pf)) v = cap;
    else v = round2(Math.min(pf, cap * 4)); // soft outlier guard
    result.push({ i, date: dateKey(trades[i]), pf: v });
  }
  return result;
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

// ─── Risk sizing (last N days) ─────────────────────────────────────────────────

function toDayIndex(dateStr: string): number {
  return Math.floor(new Date(dateStr).getTime() / 86400000);
}

export interface RiskSummary {
  /** Average P&L per trade, over trades closed in the trailing window. */
  expectancy: number;
  /** Average P&L per calendar day over the trailing window — days with no
   *  trade count as 0, so the denominator is always `days`. */
  avgDailyPnl: number;
  /** Daily loss cutoff = avgDailyPnl × 3. */
  dailyCutoff: number;
  /** Risk sized per trade = avgDailyPnl. */
  riskPerTrade: number;
  /** Trades that fell inside the window (0 means nothing to show). */
  tradeCount: number;
}

/** Expectancy + risk-sizing figures over a trailing window of calendar days,
 *  anchored to the most recent closed trade's date (not wall-clock "today") so
 *  it stays meaningful on data that isn't updated in real time. */
export function riskSummary(closed: DashboardTrade[], days = 20): RiskSummary {
  if (closed.length === 0) {
    return { expectancy: 0, avgDailyPnl: 0, dailyCutoff: 0, riskPerTrade: 0, tradeCount: 0 };
  }

  const latestDay = toDayIndex(dateKey(closed[closed.length - 1]));
  const windowStart = latestDay - (days - 1);

  const windowTrades = closed.filter((t) => {
    const d = dateKey(t);
    if (!d) return false;
    const day = toDayIndex(d);
    return day >= windowStart && day <= latestDay;
  });

  const totalPnl = windowTrades.reduce((a, t) => a + (t.pnl ?? 0), 0);
  const avgDailyPnl = round2(totalPnl / days);

  return {
    expectancy: windowTrades.length ? round2(totalPnl / windowTrades.length) : 0,
    avgDailyPnl,
    dailyCutoff: round2(avgDailyPnl * 3),
    riskPerTrade: avgDailyPnl,
    tradeCount: windowTrades.length,
  };
}
