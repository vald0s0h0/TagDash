// The unit of review is a TICKER'S DAY: every trade of that ticker in that
// session, in chronological order, played on one timeline.
//
// Keeps one such group READY ahead of time: drawn, each trade loaded and its
// window rebuilt from Alpaca SIP when the recording lacks it, the shared tape
// loaded once, the voice audio fetched. The « Revue » tab opens on it without
// waiting (it is warmed up shortly after launch), and while one group is being
// reviewed the next one is prepared behind it.
//
// Lives at module level, outside React: the tab can be left and reopened without
// losing the prepared item nor the items already seen in this run of the app.

import { api } from "@/lib/tauri";
import type { ReviewDetail } from "@/types";

export interface Prepared {
  /** The ticker's day, chronologically (empty: nothing left to review). Every
   *  detail carries the SAME tape object (the trades share their session). */
  details: ReviewDetail[];
  remaining: number;
  total: number;
  /** The SIP rebuild failed or was impossible — shown in the player toolbar. */
  enrichNote: string | null;
}

/** Items reviewed or skipped in this run — never drawn twice. */
let seen: string[] = [];
/** Items on screen (one group) — excluded from the next draw. */
let current: string[] = [];
let currentPrepared: Prepared | null = null;
let pending: Promise<Prepared> | null = null;
let generation = 0;
const audio = new Map<string, Promise<ArrayBuffer>>();

const images = new Map<string, Promise<ArrayBuffer>>();

/** An image of the trade: a TradeTally gallery path, or a local screenshot of a
 *  missed trade. Fetched once. */
export function reviewImage(src: ReviewImageSrc): Promise<ArrayBuffer> {
  const key = `${src.kind}:${src.path}`;
  let p = images.get(key);
  if (!p) {
    p = src.kind === "tt" ? api.reviewTtImage(src.path) : api.reviewScreenshot(src.path);
    p.catch(() => images.delete(key));
    images.set(key, p);
  }
  return p;
}

export interface ReviewImageSrc {
  kind: "tt" | "local";
  path: string;
}

/** The screenshots of an item: TradeTally's gallery for a taken trade, the local
 *  captures for a missed one. */
export function imageSources(d: ReviewDetail): ReviewImageSrc[] {
  if (d.item.kind === "missed") return d.screenshots.map((path) => ({ kind: "local", path }));
  const urls = d.trade?.attachment_urls;
  return Array.isArray(urls)
    ? urls.filter((u): u is string => typeof u === "string" && u.length > 0).map((path) => ({ kind: "tt", path }))
    : [];
}

/** Voice audio of a note (μ-law bytes), fetched once. */
export function voiceAudio(voiceId: string): Promise<ArrayBuffer> {
  let p = audio.get(voiceId);
  if (!p) {
    p = api.reviewVoiceAudio(voiceId);
    p.catch(() => audio.delete(voiceId));
    audio.set(voiceId, p);
  }
  return p;
}

async function prepare(exclude: string[]): Promise<Prepared> {
  const next = await api.reviewNext(exclude, null);
  const base = { remaining: next.remaining_unreviewed, total: next.total };
  if (!next.item) return { ...base, details: [], enrichNote: null };
  const items = next.group.length ? next.group : [next.item];
  // Each trade's window first (its SIP rebuild when needed), without the tape…
  const details: ReviewDetail[] = [];
  let enrichNote: string | null = null;
  for (const item of items) {
    const ref = { kind: item.kind, id: item.id };
    let d = await api.reviewLoad(ref, false);
    if (d.needs_enrichment && d.sip_available) {
      try {
        await api.reviewEnrich(ref);
        d = await api.reviewLoad(ref, false);
      } catch (e) {
        enrichNote ??= `SIP indisponible : ${String(e)}`;
      }
    } else if (d.needs_enrichment) {
      enrichNote ??= "Tape incomplet (historique SIP indisponible)";
    }
    details.push(d);
  }
  // …then the shared tape once, as it stands after every rebuild.
  const last = details[details.length - 1];
  const withTape = await api.reviewLoad({ kind: last.item.kind, id: last.item.id }, true);
  for (const d of details) d.data = withTape.data;
  const voiceIds = new Set(details.flatMap((d) => d.voices.filter((v) => v.has_audio).map((v) => v.voice_id)));
  await Promise.all([...voiceIds].map((id) => voiceAudio(id).catch(() => null)));
  // Screenshots: fetched ahead too, but never waited for (TradeTally may be slow).
  for (const d of details) for (const src of imageSources(d)) reviewImage(src).catch(() => {});
  return { ...base, details, enrichNote };
}

/** Start preparing the next item unless one is already on its way. */
export function prefetch(): void {
  if (pending) return;
  const gen = generation;
  const p = prepare([...seen, ...current]);
  pending = p;
  // A failed preparation is retried on the next take/prefetch.
  p.catch(() => {
    if (pending === p && gen === generation) pending = null;
  });
}

let taking: Promise<Prepared> | null = null;

/** The prepared item (waiting for it if still on its way); it becomes `current`
 *  and the one after it starts preparing at once. Concurrent calls share one take. */
export function take(): Promise<Prepared> {
  if (!taking) {
    taking = takeOnce().finally(() => {
      taking = null;
    });
  }
  return taking;
}

async function takeOnce(): Promise<Prepared> {
  for (;;) {
    if (!pending) prefetch();
    const p = pending!;
    const gen = generation;
    let out: Prepared;
    try {
      out = await p;
    } finally {
      if (pending === p) pending = null;
    }
    if (gen !== generation) continue; // invalidated meanwhile
    const ids = out.details.map((d) => d.item.id);
    if (ids.some((id) => seen.includes(id) || current.includes(id))) continue; // stale draw
    current = ids;
    currentPrepared = ids.length ? out : null;
    if (ids.length) prefetch();
    return out;
  }
}

/** The group on screen was reviewed or skipped. */
export function markSeen(ids: string[]): void {
  for (const id of ids) if (!seen.includes(id)) seen.push(id);
  if (ids.some((id) => current.includes(id))) {
    current = [];
    currentPrepared = null;
  }
}

/** The item still on screen when the tab was left — shown again on return. */
export function resume(): Prepared | null {
  return currentPrepared;
}

export function seenCount(): number {
  return seen.length;
}

/** New round: every item can be drawn again. */
export function resetSeen(): void {
  seen = [];
  invalidate();
}

/** Drop the prepared item (the review tags changed). */
export function invalidate(): void {
  generation++;
  pending = null;
}

/** Shortly after launch: refresh the TradeTally mirror, then prepare a trade so
 *  the tab opens on it instantly. */
export function warmUp(): void {
  api
    .syncTradetallyTrades()
    .catch(() => {})
    .finally(() => prefetch());
}
