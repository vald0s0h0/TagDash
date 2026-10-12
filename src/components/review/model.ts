// Pure data side of the review player: fills, candles, speed of tape, spread and
// the sidebar metrics, all computed from what the session recorded. Nothing here is
// estimated — a metric whose inputs are missing is `null`, never a guess.

import type { ReviewDetail, ReviewSessionData, TradeExecutions } from "@/types";

export interface PlayFill {
  ms: number;
  price: number;
  /** Signed: + buy, − sell. */
  qty: number;
  trade: string;
  sent_ms: number | null;
  ack_ms: number | null;
  recv_ms: number | null;
}

export interface OhlcBar {
  time: number; // unix s (bucket start)
  open: number;
  high: number;
  low: number;
  close: number;
  volume: number;
}

const num = (v: unknown): number | null => {
  const n = typeof v === "string" ? Number(v) : typeof v === "number" ? v : NaN;
  return Number.isFinite(n) ? n : null;
};

/** Fills with TagDash's ms timing when it placed the trade, else TradeTally's own
 *  executions (millisecond ISO timestamps). */
export function playFills(d: ReviewDetail): PlayFill[] {
  if (d.fills.length) {
    return d.fills.map((f) => ({
      ms: f.filled_ms,
      price: f.price,
      qty: f.quantity,
      trade: f.trade_id,
      sent_ms: f.sent_ms,
      ack_ms: f.ack_ms,
      recv_ms: f.recv_ms,
    }));
  }
  const ex = (d.trade?.executions as Record<string, unknown>[] | undefined) ?? [];
  return ex
    .map((e) => {
      const ms = Date.parse(String(e.datetime ?? ""));
      const q = num(e.quantity) ?? 0;
      const buy = String(e.action ?? "").toLowerCase() === "buy";
      return {
        ms,
        price: num(e.price) ?? 0,
        qty: buy ? Math.abs(q) : -Math.abs(q),
        trade: d.item.id,
        sent_ms: null,
        ack_ms: null,
        recv_ms: null,
      };
    })
    .filter((f) => Number.isFinite(f.ms) && f.price > 0)
    .sort((a, b) => a.ms - b.ms);
}

/** Executions visible at the playhead, shaped for `ExecutionsPrimitive`. A fill
 *  exists on screen only once the playhead has passed its exact millisecond — never
 *  as soon as its candle opens. */
export function visibleExecutions(
  fills: PlayFill[],
  playheadMs: number,
  sl: number | null,
  tp: number | null,
  /** Per-trade SL/TP when several trades share the replay. */
  levelsOf?: (trade: string) => { sl: number | null; tp: number | null },
): TradeExecutions[] {
  const byTrade = new Map<string, PlayFill[]>();
  for (const f of fills) {
    if (f.ms > playheadMs) continue;
    const list = byTrade.get(f.trade) ?? [];
    list.push(f);
    byTrade.set(f.trade, list);
  }
  const out: TradeExecutions[] = [];
  for (const [trade, list] of byTrade) {
    let pos = 0;
    let cash = 0;
    const execFills = list.map((f) => {
      const before = pos;
      pos += f.qty;
      cash -= f.qty * f.price;
      return {
        time: new Date(f.ms).toISOString(),
        price: f.price,
        increase: Math.abs(pos) > Math.abs(before),
        buy: f.qty > 0,
        size_pct: null,
      };
    });
    const lv = levelsOf ? levelsOf(trade) : { sl, tp };
    out.push({
      trade_id: trade,
      long: list[0].qty > 0,
      closed: pos === 0,
      pnl: pos === 0 ? cash : 0,
      original_sl: lv.sl,
      original_tp: lv.tp,
      fills: execFills,
    });
  }
  return out;
}

/** Index of the last element ≤ `x` in an ascending array (−1 when none). */
export function lastIndexAtOrBefore(arr: number[], x: number): number {
  let lo = 0;
  let hi = arr.length - 1;
  let ans = -1;
  while (lo <= hi) {
    const mid = (lo + hi) >> 1;
    if (arr[mid] <= x) {
      ans = mid;
      lo = mid + 1;
    } else hi = mid - 1;
  }
  return ans;
}

/** True when the session holds real prints (not only aggregates). */
export function hasRealPrints(data: ReviewSessionData): boolean {
  return data.trades_prints.some((n) => n === 1);
}

/** Candles of `tfSec` from the prints (and, before the first print, the 1-minute
 *  context bars), from `fromMs` (a bucket boundary) and cut at `untilMs`. */
export function buildBars(
  data: ReviewSessionData,
  tfSec: number,
  untilMs: number,
  fromMs = -Infinity,
): OhlcBar[] {
  const out: OhlcBar[] = [];
  const firstPrint = data.trades_ts.length ? data.trades_ts[0] : Infinity;
  const fromSec = fromMs / 1000;
  const push = (t: number, o: number, h: number, l: number, c: number, v: number) => {
    const bucket = Math.floor(t / tfSec) * tfSec;
    if (bucket < fromSec) return;
    const last = out[out.length - 1];
    if (last && last.time === bucket) {
      last.high = Math.max(last.high, h);
      last.low = Math.min(last.low, l);
      last.close = c;
      last.volume += v;
    } else if (!last || bucket > last.time) {
      out.push({ time: bucket, open: o, high: h, low: l, close: c, volume: v });
    }
  };
  // Context minute bars: only strictly before the tape starts (and before the
  // playhead) — after that the prints are the truth. With no tape at all they are
  // the only price path there is.
  if (tfSec >= 60 || !data.trades_ts.length) {
    for (const [t, o, h, l, c, v] of data.bars_1m) {
      if (t * 1000 >= firstPrint || t * 1000 + 60_000 > untilMs) break;
      push(t, o, h, l, c, v);
    }
  }
  const n = lastIndexAtOrBefore(data.trades_ts, untilMs);
  const first = Number.isFinite(fromMs) ? lastIndexAtOrBefore(data.trades_ts, fromMs - 1) + 1 : 0;
  for (let i = first; i <= n; i++) {
    const p = data.trades_price[i];
    push(Math.floor(data.trades_ts[i] / 1000), p, p, p, p, data.trades_size[i]);
  }
  return out;
}

export type PanelPoint = { time: number; value?: number; color?: string };

/** Speed of tape, ONE POINT PER CANDLE of the main chart (an empty candle gets a
 *  whitespace point), so the pane shares the main chart's bar indices and follows
 *  its scroll and zoom exactly: prints per second over the candle (an aggregate
 *  row counts its own trade count when it carries one, never 1 per aggregate).
 *  The spread is drawn quote by quote instead (`spreadPane.ts`). */
export function tapePanel(
  bars: OhlcBar[],
  data: ReviewSessionData,
  tfSec: number,
  untilMs: number,
  upColor: string,
  downColor: string,
): PanelPoint[] {
  const counts = new Map<number, number>();
  if (bars.length) {
    const a = lastIndexAtOrBefore(data.trades_ts, bars[0].time * 1000 - 1) + 1;
    const b = lastIndexAtOrBefore(data.trades_ts, untilMs);
    for (let i = a; i <= b; i++) {
      const k = data.trades_prints[i];
      const n = k === 1 ? 1 : k > 1 ? k : 0;
      const bucket = Math.floor(data.trades_ts[i] / 1000 / tfSec) * tfSec;
      counts.set(bucket, (counts.get(bucket) ?? 0) + n);
    }
  }
  const tape: PanelPoint[] = [];
  for (const bar of bars) {
    const n = counts.get(bar.time);
    tape.push(
      n == null
        ? { time: bar.time }
        : { time: bar.time, value: n / tfSec, color: bar.close >= bar.open ? upColor : downColor },
    );
  }
  return tape;
}

export interface SecondStat {
  time: number; // unix s
  prints: number;
  up: boolean;
}

/** Speed of tape: prints per second up to `untilMs`. An aggregate row counts its
 *  own trade count when it carries one (Massive), never 1 per aggregate. */
export function tapeSpeed(data: ReviewSessionData, untilMs: number): SecondStat[] {
  const out: SecondStat[] = [];
  const n = lastIndexAtOrBefore(data.trades_ts, untilMs);
  let prevPrice = n >= 0 ? data.trades_price[0] : 0;
  for (let i = 0; i <= n; i++) {
    const sec = Math.floor(data.trades_ts[i] / 1000);
    const k = data.trades_prints[i];
    const count = k === 1 ? 1 : k > 1 ? k : 0;
    const last = out[out.length - 1];
    const up = data.trades_price[i] >= prevPrice;
    prevPrice = data.trades_price[i];
    if (last && last.time === sec) {
      last.prints += count;
      last.up = up;
    } else out.push({ time: sec, prints: count, up });
  }
  return out;
}

export interface QuoteAt {
  bid: number;
  ask: number;
  bidSize: number | null;
  askSize: number | null;
  ms: number;
}

export function quoteAt(data: ReviewSessionData, ms: number): QuoteAt | null {
  const i = lastIndexAtOrBefore(data.quotes_ts, ms);
  if (i < 0) return null;
  return {
    bid: data.quotes_bid[i],
    ask: data.quotes_ask[i],
    bidSize: data.quotes_bid_size[i],
    askSize: data.quotes_ask_size[i],
    ms: data.quotes_ts[i],
  };
}

// ─── Sidebar metrics ────────────────────────────────────────────────────────────

export interface Metrics {
  side: "long" | "short" | null;
  entryMs: number | null;
  entryPrice: number | null;
  exitPrice: number | null;
  pnl: number | null;
  realizedR: number | null;
  maeR: number | null;
  mfeR: number | null;
  alertPrice: number | null;
  alertToEntryCents: number | null;
  alertToEntryPct: number | null;
  spreadCents: number | null;
  spreadPct: number | null;
  /** Prints/s over the 10 s before the reference instant. */
  tapeAtRef: number | null;
  /** Prints/s averaged over the seconds that printed during the session. */
  tapeAvg: number | null;
  /** Reference instant of the spread/tape readings: entry, else the alert. */
  refMs: number | null;
}

/** Stop / target of the trade: TagDash's launch-time SL, else TradeTally's levels. */
export function tradeLevels(d: ReviewDetail): { sl: number | null; tp: number | null } {
  return { sl: d.original_sl ?? num(d.trade?.stop_loss), tp: num(d.trade?.take_profit) };
}

export function computeMetrics(d: ReviewDetail, fills: PlayFill[]): Metrics {
  const t = d.trade ?? {};
  const data = d.data;
  const first = fills[0] ?? null;
  const side: Metrics["side"] = first ? (first.qty > 0 ? "long" : "short") : null;

  // Average entry / exit from the fills themselves.
  let entryQty = 0;
  let entryCost = 0;
  let exitQty = 0;
  let exitCost = 0;
  let lastExitMs: number | null = null;
  for (const f of fills) {
    const opening = side === "long" ? f.qty > 0 : f.qty < 0;
    if (opening) {
      entryQty += Math.abs(f.qty);
      entryCost += Math.abs(f.qty) * f.price;
    } else {
      exitQty += Math.abs(f.qty);
      exitCost += Math.abs(f.qty) * f.price;
      lastExitMs = f.ms;
    }
  }
  const entryPrice = entryQty ? entryCost / entryQty : num(t.entry_price);
  const exitPrice = exitQty ? exitCost / exitQty : num(t.exit_price);
  const pnl = num(t.pnl);
  const sl = d.original_sl ?? num(t.stop_loss);
  const riskPerShare = entryPrice != null && sl != null ? Math.abs(entryPrice - sl) : null;
  const qty = entryQty || num(t.quantity) || null;

  let realizedR = num(t.r_value);
  if (realizedR == null && pnl != null && riskPerShare && qty) realizedR = pnl / (riskPerShare * qty);

  // MAE / MFE from the tape between entry and last exit.
  let maeR: number | null = null;
  let mfeR: number | null = null;
  if (first && entryPrice != null && riskPerShare) {
    const end = lastExitMs ?? first.ms;
    let lo = Infinity;
    let hi = -Infinity;
    const a = lastIndexAtOrBefore(data.trades_ts, first.ms - 1) + 1;
    const b = lastIndexAtOrBefore(data.trades_ts, end);
    for (let i = a; i <= b; i++) {
      lo = Math.min(lo, data.trades_price[i]);
      hi = Math.max(hi, data.trades_price[i]);
    }
    if (Number.isFinite(lo) && Number.isFinite(hi)) {
      const adverse = side === "long" ? entryPrice - lo : hi - entryPrice;
      const favorable = side === "long" ? hi - entryPrice : entryPrice - lo;
      maeR = Math.max(0, adverse) / riskPerShare;
      mfeR = Math.max(0, favorable) / riskPerShare;
    }
  }

  const alertPrice = d.session?.alert_price ?? null;
  const alertToEntryCents =
    alertPrice != null && first ? (first.price - alertPrice) * 100 : null;
  const alertToEntryPct =
    alertPrice != null && first && alertPrice > 0 ? ((first.price - alertPrice) / alertPrice) * 100 : null;

  const refMs = first?.ms ?? d.session?.opened_ms ?? null;
  let spreadCents: number | null = null;
  let spreadPct: number | null = null;
  let tapeAtRef: number | null = null;
  if (refMs != null) {
    const q = quoteAt(data, refMs);
    if (q && q.ask > 0 && q.bid > 0) {
      spreadCents = (q.ask - q.bid) * 100;
      spreadPct = ((q.ask - q.bid) / ((q.ask + q.bid) / 2)) * 100;
    }
    if (data.trades_ts.length) {
      const a = lastIndexAtOrBefore(data.trades_ts, refMs - 10_000) + 1;
      const b = lastIndexAtOrBefore(data.trades_ts, refMs);
      let n = 0;
      for (let i = a; i <= b; i++) {
        const k = data.trades_prints[i];
        n += k === 1 ? 1 : Math.max(0, k);
      }
      tapeAtRef = n / 10;
    }
  }
  const speed = tapeSpeed(data, Infinity);
  const active = speed.filter((s) => s.prints > 0);
  const tapeAvg = active.length ? active.reduce((a, s) => a + s.prints, 0) / active.length : null;

  return {
    side,
    entryMs: first?.ms ?? null,
    entryPrice,
    exitPrice,
    pnl,
    realizedR,
    maeR,
    mfeR,
    alertPrice,
    alertToEntryCents,
    alertToEntryPct,
    spreadCents,
    spreadPct,
    tapeAtRef,
    tapeAvg,
    refMs,
  };
}

/** Timeframe the replay opens on: 10-second bars for the Micro Pullback family
 *  (Apex and Dip are its entries), 1 minute otherwise. Mirrors `review_tf_secs`. */
export function defaultTf(setup: string | null): number {
  const s = (setup ?? "").trim().toLowerCase();
  return s.includes("micro") || s === "apex" || s === "dip" ? 10 : 60;
}

/** Bars of context the replay loads around the trade. */
export const BARS_BEFORE = 50;
export const BARS_AFTER = 10;
/** Playback starts this long before the entry (or the alert of a missed trade). */
export const LEAD_MS = 5_000;

export interface PlayWindow {
  /** First bar loaded: 50 bars before the trade's bar. */
  t0: number;
  /** Last instant played: 10 bars after the exit's bar. */
  t1: number;
  /** Where playback starts by itself. */
  start: number;
}

/** What the replay covers, at timeframe `tfSec`: 50 bars before the entry, 10 bars
 *  after the exit. A missed trade is framed by its alert and its voice notes. */
export function playWindow(d: ReviewDetail, fills: PlayFill[], tfSec: number): PlayWindow | null {
  let a: number | null = fills[0]?.ms ?? null;
  let b: number | null = fills.length ? fills[fills.length - 1].ms : null;
  if (a == null) {
    const alert = d.session?.opened_ms ?? null;
    const voiceEnd = d.voices.length ? Math.max(...d.voices.map((v) => v.end_ms)) : null;
    a = alert ?? (d.data.trades_ts.length ? d.data.trades_ts[0] : null);
    b = a == null ? null : Math.max(a, voiceEnd ?? a);
  }
  if (a == null || b == null) return null;
  const tfMs = tfSec * 1000;
  const t0 = Math.floor(a / tfMs) * tfMs - BARS_BEFORE * tfMs;
  const t1 = Math.ceil((b + 1) / tfMs) * tfMs + BARS_AFTER * tfMs;
  return { t0, t1, start: Math.max(t0, a - LEAD_MS) };
}

// ─── A ticker's day on one timeline ────────────────────────────────────────────

/** The replay of a ticker's day: from 50 bars before its first trade to 10 bars
 *  after its last, starting 5 s before the first entry. */
export function groupWindow(
  details: ReviewDetail[],
  fillsPer: PlayFill[][],
  tfSec: number,
): PlayWindow | null {
  let out: PlayWindow | null = null;
  details.forEach((d, i) => {
    const w = playWindow(d, fillsPer[i] ?? [], tfSec);
    if (!w) return;
    out = out
      ? { t0: Math.min(out.t0, w.t0), t1: Math.max(out.t1, w.t1), start: Math.min(out.start, w.start) }
      : w;
  });
  return out;
}

/** When each trade of the day takes the sidebar: 5 s before its entry (its alert
 *  for a missed trade). */
export function tradeStarts(details: ReviewDetail[], fillsPer: PlayFill[][]): number[] {
  return details.map((d, i) => {
    const a = fillsPer[i]?.[0]?.ms ?? d.session?.opened_ms ?? d.item.entry_ms ?? 0;
    return a - LEAD_MS;
  });
}

/** Index of the trade on show at `ms`: the last one whose start has passed. */
export function activeTrade(starts: number[], ms: number): number {
  let k = 0;
  for (let i = 0; i < starts.length; i++) if (starts[i] <= ms) k = i;
  return k;
}
