// « Revue » tab — one past trade at a time, drawn at random from the trades whose
// TradeTally tags/setup match Settings ▸ Revue (plus the missed trades). Metrics and
// notes on the left, the replay on the right. No daily limit: « Suivant » closes
// the review and shows the next one.
//
// The unit shown is a TICKER'S DAY: all its trades on one replay timeline, the
// sidebar following the trade on screen (it moves on 5 s before the next entry).
// « Suivant » walks through the day's trades first — it closes the review of the
// trade on screen and jumps to the next one — and only draws another day after
// the last.
// Groups come READY from `queue.ts` (tapes rebuilt, voice fetched): the tab opens
// on the one prepared after launch, and the next is prepared while this one plays.

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { LoaderCircle } from "lucide-react";
import { api } from "@/lib/tauri";
import { attentionApi } from "@/attention/api";
import { useStrategies, useStrategyCards } from "@/queries/useScanner";
import type { ReviewDetail } from "@/types";
import { reviewStrategyId } from "./chart";
import { activeTrade, playFills, tradeLevels, tradeStarts } from "./model";
import { usePlayheadStore } from "./playheadStore";
import { markSeen, prefetch, resetSeen, resume, seenCount, take, type Prepared } from "./queue";
import { ReviewPlayer } from "./ReviewPlayer";
import { ReviewSidebar, type Draft } from "./ReviewSidebar";

export function ReviewView() {
  // Back on the tab: the trade left on screen, else the prepared one.
  const [shown, setShown] = useState<Prepared | null>(resume);
  const [state, setState] = useState<"loading" | "ready" | "empty" | "error">(() =>
    resume() ? "ready" : "loading",
  );
  const [error, setError] = useState<string | null>(null);
  // « Suivant » inside the day: a jump for the player to the next trade's start.
  // `n` tells a new request from the one already served.
  const [jump, setJump] = useState<{ ms: number; n: number } | null>(null);

  const show = useCallback(async () => {
    setState("loading");
    setError(null);
    try {
      const p = await take();
      // The new day starts on its first trade (the player seeks right after).
      usePlayheadStore.getState().set(0);
      setJump(null);
      setShown(p);
      setState(p.details.length ? "ready" : "empty");
    } catch (e) {
      setError(String(e));
      setState("error");
    }
  }, []);

  useEffect(() => {
    if (!resume()) show();
    // TradeTally is the source of truth: refresh the mirror behind the shown trade
    // so the next draws see today's new trades.
    api.syncTradetallyTrades().catch(() => {}).finally(() => prefetch());
  }, [show]);

  const details = useMemo(() => shown?.details ?? [], [shown]);
  const groupKey = details.map((d) => d.item.id).join("|");
  const fillsPer = useMemo(() => details.map(playFills), [details]);
  const allFills = useMemo(() => fillsPer.flat().sort((a, b) => a.ms - b.ms), [fillsPer]);
  const starts = useMemo(() => tradeStarts(details, fillsPer), [details, fillsPer]);
  // The trade on screen follows the playhead (re-renders only when it changes).
  const active = Math.min(
    usePlayheadStore((s) => activeTrade(starts, s.ms)),
    Math.max(0, details.length - 1),
  );
  const detail = details[active] ?? null;

  // Detached « Attention du marché » screen: tell it which trade is on screen and
  // where the playhead is, so it redraws the market as it stood at that instant
  // from the day's recorded map. Sent per 5-minute slot, not per frame.
  const reviewDay = detail?.item.day ?? null;
  const reviewSymbol = detail?.item.symbol ?? null;
  const reviewEntry = detail?.item.entry_ms ?? null;
  const reviewSlot = usePlayheadStore((s) => Math.floor((s.ms || reviewEntry || 0) / 300_000));
  useEffect(() => {
    if (!reviewDay || reviewSlot <= 0) return;
    // Start of the slot the playhead is in: only the slots already closed at that
    // instant are drawn — the review never sees ahead of its own clock.
    attentionApi
      .setReview({ day: reviewDay, at_ms: reviewSlot * 300_000, symbol: reviewSymbol })
      .catch(() => {});
  }, [reviewDay, reviewSymbol, reviewSlot]);
  useEffect(() => () => { attentionApi.setReview(null).catch(() => {}); }, []);

  // SL / TP of each trade, by the key its fills carry.
  const levelsOf = useMemo(() => {
    const byTrade = new Map<string, { sl: number | null; tp: number | null }>();
    details.forEach((d, i) => {
      const lv = tradeLevels(d);
      byTrade.set(d.item.id, lv);
      for (const f of fillsPer[i]) byTrade.set(f.trade, lv);
    });
    return (trade: string) => byTrade.get(trade) ?? { sl: null, tp: null };
  }, [details, fillsPer]);

  // What is typed per trade while the others play — dropped with the group.
  const drafts = useRef(new Map<string, Draft>());
  // Trades of the group whose review was closed, with the text it was closed with:
  // coming back to one (T1…) and pressing « Suivant » again adds nothing unless
  // the review was rewritten.
  const completed = useRef(new Map<string, string>());
  const draftsKey = useRef(groupKey);
  if (draftsKey.current !== groupKey) {
    draftsKey.current = groupKey;
    drafts.current = new Map();
    completed.current = new Map();
  }

  // The day's strategy card (its first trade's): its charts get that strategy's
  // pane indicators.
  const { data: cards } = useStrategyCards();
  const { data: strategies } = useStrategies();
  const strategyId = details[0] ? reviewStrategyId(details[0], strategies) : null;
  const card = (strategyId && cards?.[strategyId]) || null;

  const ids = details.map((d) => d.item.id);
  // Close one trade's review: its edited notes first, then the review appended to
  // them.
  const finish = async (d: ReviewDetail) => {
    const target = { kind: d.item.kind, id: d.item.id };
    const dr = drafts.current.get(d.item.id);
    if (dr && dr.notes != null && dr.notes !== dr.saved) {
      await api.reviewSaveNotes(target, dr.notes);
      dr.saved = dr.notes;
    }
    const review = dr?.review ?? "";
    if (completed.current.get(d.item.id) === review) return;
    await api.reviewComplete(target, review);
    completed.current.set(d.item.id, review);
  };
  // « Enregistrer » by the review: the same as closing it, without moving on. The
  // field empties and « Suivant » then adds nothing more unless a new review is typed.
  const onSaveReview = async (d: ReviewDetail): Promise<string> => {
    const target = { kind: d.item.kind, id: d.item.id };
    const dr = drafts.current.get(d.item.id);
    if (dr && dr.notes != null && dr.notes !== dr.saved) {
      await api.reviewSaveNotes(target, dr.notes);
      dr.saved = dr.notes;
    }
    const notes = await api.reviewComplete(target, dr?.review ?? "");
    if (dr) {
      dr.notes = notes;
      dr.saved = notes;
      dr.review = "";
    }
    completed.current.set(d.item.id, "");
    return notes;
  };
  const onNext = async () => {
    // Every trade of the ticker's day before another day: close this one and
    // replay the next from its start (5 s before its entry).
    if (active < details.length - 1) {
      await finish(details[active]);
      const ms = starts[active + 1];
      setJump((j) => ({ ms, n: (j?.n ?? 0) + 1 }));
      return;
    }
    // Last trade: the whole day is closed, those skipped through the T buttons too.
    for (const d of details) await finish(d);
    markSeen(ids);
    await show();
  };
  const onSkip = () => {
    markSeen(ids);
    show();
  };

  if (state === "loading") {
    return (
      <div className="flex flex-1 flex-col items-center justify-center gap-2 text-xs text-muted-foreground">
        <LoaderCircle className="h-5 w-5 animate-spin" />
        Préparation des trades…
      </div>
    );
  }
  if (state === "empty") {
    return (
      <div className="flex flex-1 flex-col items-center justify-center gap-2 text-sm text-muted-foreground">
        <p>Aucun trade à revoir.</p>
        <p className="text-xs">
          Les trades sont tirés parmi ceux dont les tags ou le setup TradeTally figurent dans Settings ▸ Revue.
        </p>
        {seenCount() > 0 && (
          <button
            type="button"
            className="mt-2 text-xs text-sky-400 hover:text-sky-300"
            onClick={() => {
              resetSeen();
              show();
            }}
          >
            Recommencer un tour
          </button>
        )}
      </div>
    );
  }
  if (state === "error" || !detail || !shown) {
    return (
      <div className="flex flex-1 flex-col items-center justify-center gap-2 text-sm text-red-400">
        {error ?? "Revue indisponible"}
        <button type="button" className="text-xs text-sky-400 hover:text-sky-300" onClick={show}>
          Réessayer
        </button>
      </div>
    );
  }

  return (
    <div className="flex min-w-0 flex-1 overflow-hidden">
      <ReviewSidebar
        key={`side-${groupKey}`}
        detail={detail}
        fills={fillsPer[active] ?? []}
        index={active}
        count={details.length}
        drafts={drafts.current}
        allFills={allFills}
        levelsOf={levelsOf}
        card={card}
        onNext={onNext}
        onSaveReview={() => onSaveReview(detail)}
        onSkip={onSkip}
      />
      <ReviewPlayer
        key={`play-${groupKey}`}
        details={details}
        fillsPer={fillsPer}
        fills={allFills}
        starts={starts}
        active={active}
        levelsOf={levelsOf}
        enrichNote={shown.enrichNote}
        remaining={shown.remaining}
        total={shown.total}
        card={card}
        jump={jump}
      />
    </div>
  );
}
