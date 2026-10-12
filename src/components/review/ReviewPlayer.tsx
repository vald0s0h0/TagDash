// Review replay — right side of the « Revue » tab.
//
// Plays a TICKER'S DAY on one timeline: every trade of that ticker in the session,
// from 50 bars before the first entry to 10 bars after the last exit (10-second
// bars for the Micro Pullback family). It starts by itself 5 s before the first
// entry once everything is drawn and the voice is loaded (the group arrives with
// its tape already rebuilt — see `queue.ts`); the sidebar follows the trade on
// screen. Like a live chart, the current bar sits on the right and the view
// advances with it (a drag to look back pauses that until « Suivre »). Everything on screen is
// cut at the PLAYHEAD: candles, volume, speed of tape, spread, bid/ask, the « Tape
// Agression » column on the right, and the execution triangles, which
// appear only once the playhead has passed their exact millisecond (never when their
// candle opens). The trader's voice plays back at the moment it was recorded, its
// transcript shown as subtitles; the silences were cut out when it was archived.
//
// The speed-of-tape pane holds ONE point per candle of the main chart and follows
// its logical range: same bar indices, so it scrolls and zooms with it. The spread
// pane draws every quote on the main chart's time axis (`spreadPane.ts`).

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  type IChartApi,
  type IPriceLine,
  type ISeriesApi,
  type SeriesMarker,
  type Time,
  type UTCTimestamp,
} from "lightweight-charts";
import {
  ChevronsRight,
  FastForward,
  MessageSquareText,
  Pause,
  Play,
  Rewind,
  SkipBack,
  Volume2,
  VolumeX,
} from "lucide-react";
import { cn } from "@/lib/utils";
import { trackUsage } from "@/lib/usageTracker";
import { nyTime } from "@/lib/nyTime";
import type { ExecutionsPrimitive } from "@/charts/executionsPrimitive";
import { BID_ASK_OPTIONS, SPREAD_TAG_COLOR, slOpts, spreadLabel, tpOpts } from "@/charts/chartOptions";
import { getChartTheme, useChartTheme } from "@/stores/chartThemeStore";
import { useBarTooltip } from "@/charts/useBarTooltip";
import type { ReviewDetail, ReviewVoiceNote, StrategyCard } from "@/types";
import {
  buildBars,
  defaultTf,
  groupWindow,
  hasRealPrints,
  quoteAt,
  tapePanel,
  visibleExecutions,
  type OhlcBar,
  type PanelPoint,
  type PlayFill,
} from "./model";
import { makeChart, paneIndicators, ReviewCandleChart, tfOf } from "./chart";
import { usePlayheadStore } from "./playheadStore";
import { SpreadPane } from "./spreadPane";
import { voiceAudio } from "./queue";
import { TapeAggression, type ReplayClock } from "./TapeAggression";
import { chunkAt, nextChunkStart, ulawToWavUrl } from "./voice";

const SPEEDS = [0.5, 1, 2, 5, 10, 30];
const TIMEFRAMES: { sec: number; label: string }[] = [
  { sec: 10, label: "10s" },
  { sec: 60, label: "1m" },
  { sec: 300, label: "5m" },
];
/** Above this replay speed the voice is muted (subtitles stay). */
const MAX_VOICE_SPEED = 2.5;
/** Fast-forward through the stretches without voice when « sauter les blancs ». */
const BLANK_SPEED = 30;
/** UI refresh while playing (charts, L1, T&S). */
const UI_MS = 120;

function clockLabel(ms: number): string {
  const d = new Date(ms);
  return `${nyTime(d, true)}.${String(d.getUTCMilliseconds()).padStart(3, "0")}`;
}

interface Props {
  /** The ticker's day, chronologically (one shared tape). */
  details: ReviewDetail[];
  /** Each trade's fills, and all of them in time order. */
  fillsPer: PlayFill[][];
  fills: PlayFill[];
  /** When each trade takes the sidebar (5 s before its entry), and the one on it. */
  starts: number[];
  active: number;
  levelsOf: (trade: string) => { sl: number | null; tp: number | null };
  /** SIP rebuild failed / impossible — shown in the toolbar. */
  enrichNote: string | null;
  /** Unreviewed items left / all items. */
  remaining: number;
  total: number;
  /** Strategy card of the day (its panes' indicators), when known. */
  card: StrategyCard | null;
  /** « Suivant » inside the day: play from `ms` (the next trade's start). A new
   *  `n` is a new request. */
  jump: { ms: number; n: number } | null;
}

interface Charts {
  rc: ReviewCandleChart;
  main: IChartApi;
  candles: ISeriesApi<"Candlestick">;
  volume: ISeriesApi<"Histogram">;
  exec: ExecutionsPrimitive;
  tape: IChartApi;
  tapeSeries: ISeriesApi<"Histogram">;
  spread: SpreadPane;
  levels: IPriceLine[];
  bidLine: IPriceLine | null;
  askLine: IPriceLine | null;
  barsLen: number;
}

export function ReviewPlayer({
  details, fillsPer, fills, starts, active, levelsOf, enrichNote, remaining, total, card, jump,
}: Props) {
  const first = details[0];
  const detail = details[active] ?? first;
  const data = first.data;
  const groupKey = details.map((d) => d.item.id).join("|");
  const [tf, setTf] = useState(() => (hasRealPrints(data) ? defaultTf(first.item.setup) : 60));
  const win = useMemo(() => groupWindow(details, fillsPer, tf), [details, fillsPer, tf]);
  // The day's voice notes (the trades share their session — de-duplicated).
  const voices: ReviewVoiceNote[] = useMemo(() => {
    const seen = new Set<string>();
    return details.flatMap((d) => d.voices).filter((v) => !seen.has(v.voice_id) && !!seen.add(v.voice_id));
  }, [details]);
  const t0 = win?.t0 ?? 0;
  const t1 = win?.t1 ?? 0;

  const [ph, setPhState] = useState(() => win?.start ?? 0);
  const phRef = useRef(win?.start ?? 0);
  const setPh = useCallback((v: number) => {
    setPhState(v);
    usePlayheadStore.getState().set(v);
  }, []);
  const [playing, setPlaying] = useState(false);
  const playingRef = useRef(false);
  const [speed, setSpeed] = useState(1);
  const speedRef = useRef(1);
  const [skipBlanks, setSkipBlanks] = useState(false);
  const skipRef = useRef(false);
  const [muted, setMuted] = useState(false);
  const mutedRef = useRef(false);

  // Shared with the Tape Agression column, read on every animation frame.
  const clock = useRef<ReplayClock>({ ph: win?.start ?? 0, speed: 1, playing: false, seq: 0 });

  useEffect(() => {
    playingRef.current = playing;
    clock.current.playing = playing;
    if (!playing) clock.current.speed = speedRef.current;
  }, [playing]);
  useEffect(() => {
    speedRef.current = speed;
    if (!playingRef.current) clock.current.speed = speed;
  }, [speed]);
  useEffect(() => { skipRef.current = skipBlanks; }, [skipBlanks]);
  useEffect(() => { mutedRef.current = muted; }, [muted]);

  // Entry / exit of each trade, for its SL/TP and the jump buttons.
  const spans = useMemo(
    () => fillsPer.map((f) => ({ entry: f[0]?.ms ?? null, exit: f.length > 1 ? f[f.length - 1].ms : null })),
    [fillsPer],
  );
  const entryMs = spans[active]?.entry ?? null;
  const exitMs = spans[active]?.exit ?? null;
  const alertMs = first.session?.opened_ms ?? null;
  const [following, setFollowing] = useState(true);

  // ── Charts ────────────────────────────────────────────────────────────────
  const mainEl = useRef<HTMLDivElement>(null);
  const tapeEl = useRef<HTMLDivElement>(null);
  const spreadEl = useRef<HTMLDivElement>(null);
  const spreadPill = useRef<HTMLDivElement>(null);
  const bidRef = useRef<number | null>(null);
  const charts = useRef<Charts | null>(null);
  // Same handles, as refs, for the shared press-and-hold bar tooltip.
  const mainChartRef = useRef<IChartApi | null>(null);
  const candleRef = useRef<ISeriesApi<"Candlestick"> | null>(null);
  const volumeRef = useRef<ISeriesApi<"Histogram"> | null>(null);

  useEffect(() => {
    if (!mainEl.current || !tapeEl.current || !spreadEl.current) return;
    const withSec = tf < 60;
    // Dressed like the trading pane of this timeframe (TagDash defaults + the
    // strategy card's indicators).
    const rc = new ReviewCandleChart(mainEl.current, withSec, paneIndicators(card, tfOf(tf)));
    rc.onFollowChange = setFollowing;
    setFollowing(true);
    const { chart: main, candles, volume, exec } = rc;
    mainChartRef.current = main;
    candleRef.current = candles;
    volumeRef.current = volume;

    // Followers: no scroll/zoom of their own, they mirror the main chart.
    const tape = makeChart(tapeEl.current, withSec, { interactive: false, timeVisible: false });
    const tapeSeries = tape.addHistogramSeries({
      priceFormat: { type: "price", precision: 1, minMove: 0.1 },
      priceLineVisible: false,
      lastValueVisible: true,
      title: "prints/s",
    });
    const spread = new SpreadPane(spreadEl.current);

    // Same plot width as the main chart (its price scale grows with the price), so
    // the followers' bars sit exactly under the candles.
    let scaleW = 0;
    const follow = () => {
      const r = main.timeScale().getVisibleLogicalRange();
      const w = main.priceScale("right").width();
      if (w > 0 && w !== scaleW) {
        scaleW = w;
        tape.applyOptions({ rightPriceScale: { minimumWidth: w } });
      }
      if (r) tape.timeScale().setVisibleLogicalRange(r);
      spread.schedule();
    };
    main.timeScale().subscribeVisibleLogicalRangeChange(follow);
    main.timeScale().subscribeSizeChange(follow);

    charts.current = {
      rc, main, candles, volume, exec, tape, tapeSeries, spread,
      levels: [], bidLine: null, askLine: null, barsLen: 0,
    };
    return () => {
      charts.current = null;
      mainChartRef.current = null;
      candleRef.current = null;
      volumeRef.current = null;
      rc.remove();
      tape.remove();
      spread.destroy();
    };
    // Rebuilt per day and per timeframe (seconds on the axis change with it).
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [details, tf]);

  // Press-and-hold: volume and body % of the bar under the cursor, as on the
  // trading panes.
  useBarTooltip(mainChartRef, candleRef, volumeRef, mainEl, `${groupKey}-${tf}`);

  // The strategy card may arrive after the chart was built.
  useEffect(() => {
    charts.current?.rc.setIndicators(paneIndicators(card, tfOf(tf)));
  }, [card, tf]);

  // Settings ▸ Apparence edits apply live, as on the trading panes.
  const theme = useChartTheme();
  useEffect(() => {
    charts.current?.rc.applyTheme(theme);
  }, [theme]);

  // The spread pill rides the bid line, where its own price tag would be — same
  // reading as the trading charts: price on the ask, cost of crossing on the bid.
  useEffect(() => {
    let raf = 0;
    const tick = () => {
      const node = spreadPill.current;
      const c = charts.current;
      if (node) {
        const y = c && bidRef.current != null ? c.candles.priceToCoordinate(bidRef.current) : null;
        if (y == null) node.style.display = "none";
        else {
          node.style.display = "flex";
          node.style.top = `${y}px`;
        }
      }
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  }, []);
  const [spreadText, setSpreadText] = useState<string | null>(null);

  // Draw everything up to the playhead.
  const render = useCallback(
    (at: number, full: boolean) => {
      const c = charts.current;
      if (!c) return;
      const theme = getChartTheme();
      const bars: OhlcBar[] = buildBars(data, tf, at, t0);
      const tapePoints = tapePanel(bars, data, tf, at, `${theme.candle.up}aa`, `${theme.candle.down}aa`);
      const toPoint = (p: PanelPoint) => ({ ...p, time: p.time as UTCTimestamp });
      c.rc.setBars(bars, full);
      if (full || bars.length < c.barsLen) {
        c.tapeSeries.setData(tapePoints.map(toPoint));
      } else {
        for (let i = Math.max(0, c.barsLen - 1); i < bars.length; i++) c.tapeSeries.update(toPoint(tapePoints[i]));
      }
      c.barsLen = bars.length;
      c.spread.update({ main: c.main, barTimes: bars.map((b) => b.time), tfSec: tf, data, at });

      // Executions — cut at the playhead to the millisecond, each trade with its
      // own SL/TP segments.
      c.exec.setData(visibleExecutions(fills, at, null, null, levelsOf), bars.map((b) => b.time));

      // SL / TP of whichever trade is on at the playhead.
      for (const l of c.levels) c.candles.removePriceLine(l);
      c.levels = [];
      spans.forEach((sp, i) => {
        if (sp.entry == null || at < sp.entry || (sp.exit != null && at > sp.exit)) return;
        const { sl, tp } = levelsOf(details[i].item.id);
        // The trading panes' order styling (a position is open); the native axis
        // tag stands in for their « price + ✕ » pill.
        if (sl != null) c.levels.push(c.candles.createPriceLine({ price: sl, ...slOpts(true), axisLabelVisible: true }));
        if (tp != null) c.levels.push(c.candles.createPriceLine({ price: tp, ...tpOpts(true), axisLabelVisible: true }));
      });

      // Bid / ask at the playhead: the ask keeps its price tag, the bid shows the
      // spread (pill tracked above).
      const q = quoteAt(data, at);
      const quoted = !!q && q.bid > 0 && q.ask > 0;
      // No quotes (Massive Starter): the native last-price line instead, as on the
      // trading panes.
      c.candles.applyOptions({ priceLineVisible: !quoted, lastValueVisible: !quoted });
      if (q && quoted) {
        if (c.askLine) c.askLine.applyOptions({ price: q.ask });
        else c.askLine = c.candles.createPriceLine({ price: q.ask, ...BID_ASK_OPTIONS, title: "" });
        if (c.bidLine) c.bidLine.applyOptions({ price: q.bid });
        else c.bidLine = c.candles.createPriceLine({ price: q.bid, ...BID_ASK_OPTIONS, axisLabelVisible: false });
        bidRef.current = q.bid;
        setSpreadText(spreadLabel(q.bid, q.ask));
      } else {
        if (c.askLine) c.candles.removePriceLine(c.askLine);
        if (c.bidLine) c.candles.removePriceLine(c.bidLine);
        c.askLine = null;
        c.bidLine = null;
        bidRef.current = null;
        setSpreadText(null);
      }

      // Alert marker, snapped to the candle holding it, once reached.
      const markers: SeriesMarker<Time>[] = [];
      if (alertMs != null && at >= alertMs && bars.length) {
        const sec = Math.floor(alertMs / 1000);
        const bar = [...bars].reverse().find((b) => b.time <= sec);
        if (bar)
          markers.push({ time: bar.time as UTCTimestamp, position: "aboveBar", shape: "arrowDown", color: "#facc15", text: "Alerte" });
      }
      c.candles.setMarkers(markers);
    },
    [data, tf, t0, fills, spans, levelsOf, details, alertMs],
  );

  const seek = useCallback(
    (ms: number) => {
      const v = Math.min(Math.max(ms, t0), t1);
      phRef.current = v;
      clock.current.ph = v;
      clock.current.seq++;
      setPh(v);
      render(v, true);
      // A jump puts the current bar back on the right.
      charts.current?.rc.follow();
    },
    [t0, t1, render, setPh],
  );
  // Full redraw on item / timeframe change (the playhead is kept, clamped).
  useEffect(() => {
    seek(phRef.current);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [render]);

  // « Suivant » to the day's next trade: from its start, playing — the way the day
  // itself opens. Each request is served once (`seek` changes with the timeframe).
  const servedJump = useRef(jump?.n ?? 0);
  useEffect(() => {
    if (!jump || jump.n === servedJump.current) return;
    servedJump.current = jump.n;
    seek(jump.ms);
    setPlaying(true);
  }, [jump, seek]);

  // ── Voice ────────────────────────────────────────────────────────────────
  const audios = useRef(new Map<string, HTMLAudioElement>());
  const [voicesReady, setVoicesReady] = useState(false);
  useEffect(() => {
    const urls: string[] = [];
    let cancelled = false;
    setVoicesReady(false);
    Promise.all(
      voices
        .filter((v) => v.has_audio)
        .map((v) =>
          voiceAudio(v.voice_id).then((buf) => {
            if (cancelled) return;
            const url = ulawToWavUrl(buf);
            urls.push(url);
            const a = new Audio(url);
            a.preload = "auto";
            (a as HTMLAudioElement & { preservesPitch?: boolean }).preservesPitch = true;
            audios.current.set(v.voice_id, a);
          }, () => {}),
        ),
    ).then(() => !cancelled && setVoicesReady(true));
    const map = audios.current;
    return () => {
      cancelled = true;
      for (const a of map.values()) a.pause();
      map.clear();
      urls.forEach((u) => URL.revokeObjectURL(u));
    };
  }, [voices]);

  // The day opens 5 s before its first entry and plays by itself — once the bars
  // are drawn (next frame) and the voice is loaded.
  useEffect(() => {
    if (!win || !voicesReady) return;
    seek(win.start);
    let raf = requestAnimationFrame(() => {
      raf = requestAnimationFrame(() => setPlaying(true));
    });
    return () => cancelAnimationFrame(raf);
    // Only when the day is ready — not on a timeframe switch.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [details, voicesReady]);

  const syncVoice = useCallback((at: number) => {
    const act = chunkAt(voices, at);
    const sp = speedRef.current;
    for (const [id, a] of audios.current) {
      const mine = act && act.voice.voice_id === id;
      if (!mine || !playingRef.current || mutedRef.current || sp > MAX_VOICE_SPEED) {
        if (!a.paused) a.pause();
        continue;
      }
      a.playbackRate = Math.min(4, Math.max(0.25, sp / Math.max(0.1, act.voice.speed)));
      if (a.paused) {
        a.currentTime = act.audioSec;
        a.play().catch(() => {});
      } else if (Math.abs(a.currentTime - act.audioSec) > 0.35) {
        a.currentTime = act.audioSec;
      }
    }
  }, [voices]);

  // Play = back to the current bar.
  useEffect(() => {
    if (playing) charts.current?.rc.follow();
  }, [playing]);

  // ── Clock ────────────────────────────────────────────────────────────────
  useEffect(() => {
    if (!playing) {
      syncVoice(phRef.current);
      return;
    }
    let raf = 0;
    let last = performance.now();
    let lastUi = 0;
    const tick = (now: number) => {
      const dt = now - last;
      last = now;
      let eff = speedRef.current;
      if (skipRef.current && !chunkAt(voices, phRef.current)) {
        const next = nextChunkStart(voices, phRef.current);
        if (next != null) eff = Math.max(eff, BLANK_SPEED);
      }
      let next = phRef.current + dt * eff;
      if (next >= t1) {
        next = t1;
        setPlaying(false);
      }
      phRef.current = next;
      clock.current.ph = next;
      clock.current.speed = eff;
      syncVoice(next);
      if (now - lastUi >= UI_MS || next >= t1) {
        lastUi = now;
        setPh(next);
        render(next, false);
      }
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  }, [playing, t1, render, syncVoice, voices, setPh]);

  // Keyboard: space = play/pause, ←/→ = ∓5 s (not while typing a note). Caught
  // first (capture phase) so nothing else reacts, and a focused button (the last
  // one clicked) is not « clicked » again by the space bar on key release.
  useEffect(() => {
    const typing = (el: Element | null) => {
      const tag = (el?.tagName ?? "").toLowerCase();
      return tag === "textarea" || tag === "input" || tag === "select" || (el as HTMLElement | null)?.isContentEditable;
    };
    // An open image modal takes the keys.
    const modal = () => !!document.querySelector("[data-review-modal]");
    const onKey = (e: KeyboardEvent) => {
      if (typing(document.activeElement) || modal()) return;
      if (e.code === "Space") {
        e.preventDefault();
        e.stopPropagation();
        if (e.repeat) return;
        (document.activeElement as HTMLElement | null)?.blur?.();
        trackUsage("key", "review_play_pause", { context: "review", input: "Espace" });
        setPlaying((p) => !p);
      } else if (e.code === "ArrowRight" || e.code === "ArrowLeft") {
        e.preventDefault();
        e.stopPropagation();
        // A held arrow is one gesture, not thirty.
        if (!e.repeat) {
          const fwd = e.code === "ArrowRight";
          trackUsage("key", fwd ? "review_seek_forward" : "review_seek_back",
            { context: "review", input: fwd ? "→" : "←" });
        }
        seek(phRef.current + (e.code === "ArrowRight" ? 5_000 : -5_000));
      }
    };
    const onKeyUp = (e: KeyboardEvent) => {
      if (e.code === "Space" && !typing(document.activeElement) && !modal()) {
        e.preventDefault();
        e.stopPropagation();
      }
    };
    window.addEventListener("keydown", onKey, true);
    window.addEventListener("keyup", onKeyUp, true);
    return () => {
      window.removeEventListener("keydown", onKey, true);
      window.removeEventListener("keyup", onKeyUp, true);
    };
  }, [seek]);

  // ── Derived UI at the playhead ────────────────────────────────────────────
  const subtitle = useMemo(() => {
    for (const v of voices)
      for (const s of v.subtitles) if (ph >= s.start_ms && ph <= s.end_ms + 1500) return s.text;
    return null;
  }, [voices, ph]);
  const nextVoice = nextChunkStart(voices, ph);

  if (!win) {
    return (
      <div className="flex flex-1 items-center justify-center text-sm text-muted-foreground">
        Aucune donnée de marché pour ce trade{first.sip_available ? "." : " (historique SIP indisponible)."}
      </div>
    );
  }

  const origins = data.origins.filter((o) => o !== "context");

  return (
    <div className="flex min-w-0 flex-1 flex-col overflow-hidden">
      {/* Transport */}
      <div className="flex flex-wrap items-center gap-1 border-b border-border px-2 py-1.5 text-xs">
        <Btn title="Début (5 s avant la première entrée)" onClick={() => seek(win.start)}><SkipBack className="h-3.5 w-3.5" /></Btn>
        <Btn title="−10 s (←)" onClick={() => seek(ph - 10_000)}><Rewind className="h-3.5 w-3.5" /></Btn>
        <Btn title="Lecture / pause (espace)" onClick={() => setPlaying((p) => !p)} active={playing}>
          {playing ? <Pause className="h-3.5 w-3.5" /> : <Play className="h-3.5 w-3.5" />}
        </Btn>
        <Btn title="+10 s (→)" onClick={() => seek(ph + 10_000)}><FastForward className="h-3.5 w-3.5" /></Btn>
        <span className="mx-1 font-mono tabular-nums text-foreground">{clockLabel(ph)}</span>
        <Sep />
        {SPEEDS.map((s) => (
          <Btn key={s} onClick={() => setSpeed(s)} active={speed === s}>{s}×</Btn>
        ))}
        <Sep />
        {TIMEFRAMES.map((t) => (
          <Btn key={t.sec} onClick={() => setTf(t.sec)} active={tf === t.sec}>{t.label}</Btn>
        ))}
        <Sep />
        {alertMs != null && <Btn onClick={() => seek(alertMs - 5_000)}>Alerte</Btn>}
        {details.length > 1 &&
          starts.map((st, i) => (
            <Btn
              key={details[i].item.id}
              title={`Trade ${i + 1} de ${first.item.symbol} ce jour-là (5 s avant son entrée)`}
              onClick={() => seek(st)}
              active={i === active}
            >
              T{i + 1}
            </Btn>
          ))}
        {entryMs != null && <Btn title="Entrée du trade affiché" onClick={() => seek(entryMs - 5_000)}>Entrée</Btn>}
        {exitMs != null && <Btn title="Sortie du trade affiché" onClick={() => seek(exitMs - 5_000)}>Sortie</Btn>}
        {voices.length > 0 && (
          <>
            <Btn title="Commentaire suivant" onClick={() => nextVoice != null && seek(nextVoice)}>
              <MessageSquareText className="h-3.5 w-3.5" />
            </Btn>
            <Btn title="Sauter les blancs (avance rapide entre les commentaires)" onClick={() => setSkipBlanks((v) => !v)} active={skipBlanks}>
              Sauter les blancs
            </Btn>
            <Btn title={muted ? "Voix coupée" : "Voix"} onClick={() => setMuted((m) => !m)}>
              {muted ? <VolumeX className="h-3.5 w-3.5" /> : <Volume2 className="h-3.5 w-3.5" />}
            </Btn>
          </>
        )}
        <div className="flex-1" />
        {enrichNote && <span className="text-[10px] text-amber-400">{enrichNote}</span>}
        <span className="text-[10px] text-muted-foreground" title="Origine des données de la session">
          {origins.length ? origins.join(" · ") : "aucun tape enregistré"}
        </span>
        <Sep />
        <span className="text-[10px] text-muted-foreground">
          {remaining} non revu{remaining > 1 ? "s" : ""} / {total}
          {detail.item.review_count > 0 && ` · revu ${detail.item.review_count}×`}
        </span>
      </div>

      <div className="flex min-h-0 flex-1">
        {/* Charts */}
        <div className="flex min-w-0 flex-1 flex-col">
          <div className="relative min-h-0 flex-[5]">
            <div ref={mainEl} className="absolute inset-0" />
            {!following && (
              <button
                type="button"
                onClick={() => charts.current?.rc.follow()}
                title="Revenir à la barre courante et la suivre"
                className="absolute right-20 top-2 z-20 flex items-center gap-1 rounded bg-sky-600/90 px-2 py-0.5 text-[11px] text-white hover:bg-sky-500"
              >
                <ChevronsRight className="h-3.5 w-3.5" /> Suivre
              </button>
            )}
            {spreadText != null && (
              <div
                ref={spreadPill}
                title="Spread (ask − bid)"
                style={{
                  right: 2, top: -100, transform: "translateY(-50%)",
                  borderColor: "rgba(200,200,200,0.35)", color: SPREAD_TAG_COLOR, display: "none",
                }}
                className="pointer-events-none absolute z-20 flex items-center rounded-sm border bg-black/80 px-1 py-px text-[9px] leading-none tabular-nums"
              >
                {spreadText}
              </div>
            )}
            {subtitle && (
              <div className="pointer-events-none absolute inset-x-6 bottom-6 z-10 flex justify-center">
                <span className="max-w-[80%] rounded bg-black/75 px-3 py-1.5 text-center text-sm leading-snug text-white">
                  {subtitle}
                </span>
              </div>
            )}
          </div>
          <div className="relative min-h-0 flex-[1.3] border-t border-border">
            <div ref={tapeEl} className="absolute inset-0" />
          </div>
          <div className="relative min-h-0 flex-[1.1] border-t border-border">
            <div ref={spreadEl} className="absolute inset-0" />
          </div>
        </div>

        <TapeAggression data={data} fills={fills} clock={clock} />
      </div>

      {/* Scrubber with alert / fills / voice markers */}
      <Scrubber
        t0={t0}
        t1={t1}
        ph={ph}
        onSeek={seek}
        voices={voices.flatMap((v) =>
          v.chunks.map((c) => ({ start: c.start_ms, end: c.start_ms + (c.samples / 16) * v.speed })),
        )}
        alertMs={alertMs != null && alertMs >= t0 ? alertMs : null}
        fills={fills}
      />
    </div>
  );
}

/** Timeline at the foot of the player: a thin zinc rail, the elapsed part lit,
 *  voice chunks as sky segments on the rail, alert / fills as ticks across it, and
 *  a white playhead. Click or drag anywhere on the strip to seek; hovering shows
 *  the time under the pointer. */
function Scrubber({
  t0, t1, ph, onSeek, voices, alertMs, fills,
}: {
  t0: number;
  t1: number;
  ph: number;
  onSeek: (ms: number) => void;
  voices: { start: number; end: number }[];
  alertMs: number | null;
  fills: PlayFill[];
}) {
  const railRef = useRef<HTMLDivElement>(null);
  const [hover, setHover] = useState<number | null>(null);
  const [dragging, setDragging] = useState(false);
  const span = Math.max(1, t1 - t0);
  const pct = (ms: number) => Math.min(100, Math.max(0, ((ms - t0) / span) * 100));

  const msAt = (clientX: number) => {
    const r = railRef.current?.getBoundingClientRect();
    if (!r || r.width <= 0) return t0;
    const f = Math.min(1, Math.max(0, (clientX - r.left) / r.width));
    return Math.round(t0 + f * span);
  };

  return (
    <div className="flex shrink-0 items-center gap-3 border-t border-border bg-card px-3 py-1.5">
      <span className="font-mono text-[10px] tabular-nums text-muted-foreground">{nyTime(new Date(t0), true)}</span>
      <div
        ref={railRef}
        data-track="Timeline revue"
        className="group relative h-6 flex-1 cursor-pointer touch-none select-none"
        onPointerDown={(e) => {
          e.currentTarget.setPointerCapture(e.pointerId);
          setDragging(true);
          onSeek(msAt(e.clientX));
        }}
        onPointerMove={(e) => {
          const ms = msAt(e.clientX);
          setHover(ms);
          if (dragging) onSeek(ms);
        }}
        onPointerUp={(e) => {
          e.currentTarget.releasePointerCapture(e.pointerId);
          setDragging(false);
        }}
        onPointerLeave={() => setHover(null)}
      >
        {/* Rail + elapsed part */}
        <div className="absolute inset-x-0 top-1/2 h-1 -translate-y-1/2 overflow-hidden rounded-full bg-muted transition-[height] group-hover:h-1.5">
          <div className="absolute inset-y-0 left-0 bg-foreground/30" style={{ width: `${pct(ph)}%` }} />
          {voices.map((v, i) => (
            <span
              key={i}
              className="absolute inset-y-0 bg-sky-400/70"
              style={{ left: `${pct(v.start)}%`, width: `${Math.max(0.3, pct(v.end) - pct(v.start))}%` }}
            />
          ))}
        </div>

        {/* Alert / fill ticks */}
        {alertMs != null && (
          <span
            className="pointer-events-none absolute top-1/2 h-3 w-0.5 -translate-x-1/2 -translate-y-1/2 rounded-full bg-yellow-400"
            style={{ left: `${pct(alertMs)}%` }}
          />
        )}
        {fills.map((f, i) => (
          <span
            key={i}
            className={cn(
              "pointer-events-none absolute top-1/2 h-3 w-0.5 -translate-x-1/2 -translate-y-1/2 rounded-full",
              f.qty > 0 ? "bg-emerald-400" : "bg-red-400",
            )}
            style={{ left: `${pct(f.ms)}%` }}
          />
        ))}

        {/* Hover time */}
        {hover != null && !dragging && (
          <>
            <span
              className="pointer-events-none absolute top-1/2 h-3 w-px -translate-x-1/2 -translate-y-1/2 bg-foreground/40"
              style={{ left: `${pct(hover)}%` }}
            />
            <span
              className="pointer-events-none absolute -top-5 z-10 -translate-x-1/2 rounded border border-border bg-popover px-1 py-px font-mono text-[10px] tabular-nums text-foreground shadow"
              style={{ left: `${pct(hover)}%` }}
            >
              {nyTime(new Date(hover), true)}
            </span>
          </>
        )}

        {/* Playhead */}
        <span
          className="pointer-events-none absolute top-1/2 h-4 w-[2px] -translate-x-1/2 -translate-y-1/2 rounded-full bg-foreground shadow-[0_0_0_2px_hsl(var(--card))]"
          style={{ left: `${pct(ph)}%` }}
        />
      </div>
      <span className="font-mono text-[10px] tabular-nums text-muted-foreground">{nyTime(new Date(t1), true)}</span>
    </div>
  );
}

function Btn({
  children, onClick, active, title,
}: { children: React.ReactNode; onClick: () => void; active?: boolean; title?: string }) {
  return (
    <button
      type="button"
      title={title}
      onClick={onClick}
      className={cn(
        "flex h-6 items-center rounded px-1.5 text-[11px] transition-colors",
        active ? "bg-accent text-foreground" : "text-muted-foreground hover:bg-accent hover:text-foreground",
      )}
    >
      {children}
    </button>
  );
}

function Sep() {
  return <span className="mx-1 h-4 w-px bg-border" />;
}
