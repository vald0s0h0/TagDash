import { useState, useRef, useEffect, useCallback, Fragment } from "react";
import {
  AlarmClockPlus,
  Bird,
  Camera,
  CheckCircle,
  ChevronDown,
  CircleX,
  Clock,
  LoaderCircle,
  MoreHorizontal,
  NotebookPen,
  Slash,
  Smile,
  Type,
} from "lucide-react";
import { JournalModal } from "./JournalModal";
import { MicDictate } from "./MicDictate";
import { DrawingContextMenu } from "./DrawingContextMenu";
import { useQuery } from "@tanstack/react-query";
import type { PaneSpec, StrategyCard, Timeframe, ZoneAssignment } from "@/types";
import { useLayoutStore, MANUAL_STRATEGY_ID } from "@/stores/layoutStore";
import {
  useChartStore, type DrawMode, type DrawScope, type ChartLine,
  type ChartAnnotation, type CtxTarget, type LineStyleName,
} from "@/stores/chartStore";
import { useDrawingPrefs } from "@/stores/drawingPrefsStore";
import { useChartTheme } from "@/stores/chartThemeStore";
import { usePaneSizeStore } from "@/stores/paneSizeStore";
import {
  registerZoneHotkeys, setHoveredZone, TF_FOR_ACTION, type HotkeyActionId,
} from "@/stores/hotkeyStore";
import { registerZoneGamepad, getChartControl } from "@/lib/gamepadBus";
import { useStrategyCards } from "@/queries/useScanner";
import { api } from "@/lib/tauri";
import { createCrosshairSync } from "@/lib/crosshairSync";
import { nyFilenameStamp } from "@/lib/nyTime";
import { LightweightChart } from "./LightweightChart";
import { cn } from "@/lib/utils";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import {
  PRIORITY_STYLES, TIMEFRAMES,
  ChartInfoBar, StrategyInfoOverlay, MicroInfoOverlay, HodDriveInfoOverlay, TBtn, Sep, EmptyZone,
} from "./chartZoneParts";

// Micro Pullback gets the rich on-chart risk overlay (on its sub-minute pane);
// every other strategy uses the generic field overlay (on its left pane).
const MICRO_PULLBACK_ID = "micro_pullback";
// HOD Drive gets its own KPI overlay + HOD/LOD points + green-series crosses on its
// timeframe (right/interactive) pane.
const HOD_DRIVE_ID = "hod_drive";

// Mood emojis offered by the toolbar (euphoric / angry / panicked / in the clouds).
const EMOJIS: { glyph: string; title: string }[] = [
  { glyph: "🤑", title: "Euphorique" },
  { glyph: "😡", title: "En colère" },
  { glyph: "😱", title: "Paniqué" },
  { glyph: "😶‍🌫️", title: "Dans les nuages (oubli des règles)" },
];

// Default chart for a manually-searched ticker: one 5-minute pane with VWAP +
// Bollinger, info band = float / industry / country.
const MANUAL_CARD: StrategyCard = {
  universe: "us_stocks",
  panes: [{
    timeframe: "5m",
    symbol: null,
    interactive: true,
    indicators: [
      { kind: "vwap", period: null },
      { kind: "bollinger_bands", period: 20 },
    ],
  }],
  info_fields: [
    { key: "float_shares", label: "Float",     source: "enrichment" },
    { key: "industry",     label: "Industrie", source: "enrichment" },
    { key: "country",      label: "Pays",      source: "enrichment" },
    { key: "price",        label: "Px",        source: "alert" },
    { key: "change_day_pct", label: "Chg",     source: "alert" },
  ],
  llm: null,
  enrichments: [],
};

// ─── Main ChartZone ───────────────────────────────────────────────────────────

interface ChartZoneProps {
  zone: ZoneAssignment;
}

export function ChartZone({ zone }: ChartZoneProps) {
  const headerRef   = useRef<HTMLDivElement>(null);
  const chartAreaRef = useRef<HTMLDivElement>(null);
  const infoBandRef  = useRef<HTMLDivElement>(null);
  // One crosshair-sync group per zone: hovering a pane mirrors the crosshair on
  // the zone's other same-instrument panes.
  const crosshairSyncRef = useRef(createCrosshairSync());
  const [narrowToolbar, setNarrowToolbar] = useState(false);
  // Split-day marker colour (the rest of the chart palette is read inside the
  // chart itself); subscribing here re-renders when the palette is edited.
  const chartTheme = useChartTheme();

  // Journal modal — pinned to the trade it was opened for. The mono-chart zone
  // can switch tickers underneath an open journal; capturing the target here
  // keeps the modal (and any in-progress notes) on the original trade instead
  // of reloading the newly-arrived ticker's entry.
  const [journalTarget, setJournalTarget] =
    useState<{ tradeId: string; symbol: string } | null>(null);

  // Inline voice-dictation toggle, registered by the toolbar MicDictate button so
  // the Xbox `journal_audio` button can start/stop the same recording.
  const audioToggleRef = useRef<(() => void) | null>(null);

  // Capture status: idle | pending | success | error
  const [captureStatus, setCaptureStatus] = useState<"idle" | "pending" | "success" | "error">("idle");

  const releaseZone      = useLayoutStore((s) => s.releaseZone);

  const chartStore   = useChartStore();
  const zoneState    = useChartStore((s) => s.getZone(zone.zone_id));
  const { timeframe, drawMode, orderMode, lines, annotations, alarms, linePoint1, pendingEmoji, context } = zoneState;

  // Right-click context menu (one per zone).
  const [ctxMenu, setCtxMenu] = useState<{ target: CtxTarget; x: number; y: number } | null>(null);

  const hasSl      = context?.stop_loss  != null;
  const hasTp      = context?.take_profit != null;
  const hasTradeId = !!context?.trade_id;

  // Snapshot the current trade and open the journal pinned to it.
  const openJournal = useCallback(() => {
    const tradeId = context?.trade_id;
    if (tradeId && zone.symbol) setJournalTarget({ tradeId, symbol: zone.symbol });
  }, [context?.trade_id, zone.symbol]);

  // Toggle the inline voice dictation for the current trade (gamepad path mirrors
  // the toolbar mic button).
  const toggleAudioNote = useCallback(() => {
    audioToggleRef.current?.();
  }, []);

  // ── Strategy identity card → panes + info-band fields ──────────────────────
  const { data: cards } = useStrategyCards();
  const isManual = zone.strategy_id === MANUAL_STRATEGY_ID;
  const card  = isManual
    ? MANUAL_CARD
    : zone.strategy_id ? cards?.[zone.strategy_id] ?? null : null;
  // Panes to render (fallback = a single interactive pane at the toolbar tf).
  const panes: PaneSpec[] = card?.panes?.length
    ? card.panes
    : [{ timeframe, symbol: null, indicators: [], interactive: true }];
  // The pane that carries SL/TP/orders/drawing (the 5s pane for micro_pullback).
  const interactiveIdx = Math.max(0, panes.findIndex((p) => p.interactive));

  // ── Controller chart focus (R1 cycles which pane the sticks drive) ──────────
  // Default = the interactive pane; re-seeded when the pane layout (strategy) or
  // ticker changes. A ref mirrors it so the gamepad registration reads it live
  // without re-registering on every focus change.
  const [focusPaneIdx, setFocusPaneIdx] = useState(interactiveIdx);
  const focusPaneIdxRef = useRef(focusPaneIdx);
  useEffect(() => { focusPaneIdxRef.current = focusPaneIdx; });
  useEffect(() => { setFocusPaneIdx(interactiveIdx); }, [interactiveIdx, zone.symbol]);

  // Layout columns: panes sharing a `column` stack vertically (declaration
  // order); panes without one each get their own column. Legacy cards (no
  // column) → the previous side-by-side layout. Micro Pullback uses this to put
  // daily + 5m in the left column and the 10s pane in the right.
  const columns: number[][] = (() => {
    const order: (number | string)[] = [];
    const map = new Map<number | string, number[]>();
    panes.forEach((p, i) => {
      const key = p.column ?? `solo-${i}`;
      if (!map.has(key)) { map.set(key, []); order.push(key); }
      map.get(key)!.push(i);
    });
    return order.map((k) => map.get(k)!);
  })();
  // Pane that carries the strategy info overlay: Micro Pullback draws its rich
  // risk overlay on the sub-minute (interactive) pane; other strategies keep the
  // generic field overlay on the left-most pane.
  const isMicro = zone.strategy_id === MICRO_PULLBACK_ID;
  const isHod   = zone.strategy_id === HOD_DRIVE_ID;
  // Micro Pullback + HOD Drive draw their overlay on the interactive (right) pane;
  // every other strategy keeps the generic field overlay on the left-most pane.
  const overlayPaneIdx = isMicro || isHod ? interactiveIdx : 0;

  // ── Resizable panes (drag the gutters BETWEEN panes / columns) ─────────────
  // Sizes are flex-grow ratios persisted per layout shape (see paneSizeStore), so a
  // split survives zone reassignments and is shared by charts of the same strategy.
  // Only internal gutters resize — never the zone's outer edges.
  const layoutKey = `${zone.strategy_id ?? "manual"}|${columns.map((c) => c.length).join("-")}`;
  const paneSizes = usePaneSizeStore((s) => s.byLayout[layoutKey]);
  const setColSizes = usePaneSizeStore((s) => s.setCols);
  const setRowSizes = usePaneSizeStore((s) => s.setRows);
  const colGrow = (ci: number) => paneSizes?.cols?.[ci] ?? 1;
  const rowGrow = (ci: number, ri: number) => paneSizes?.rows?.[ci]?.[ri] ?? 1;

  // Active gutter drag (window-level move/up while set). A full snapshot of the
  // adjacent grows is captured at mousedown so the move handler never depends on the
  // live store (no listener re-bind per move). axis x = between columns (ci = left
  // index); y = between panes of column ci (ri = top index).
  const gutterDrag = useRef<
    | {
        axis: "x" | "y";
        ci: number;
        ri: number;
        start: number;
        size0: number;
        size1: number;
        sumGrow: number;
        cols: number[];
        rows: number[];
      }
    | null
  >(null);
  const [gutterActive, setGutterActive] = useState(false);

  const onGutterDown = useCallback(
    (e: React.MouseEvent, axis: "x" | "y", ci: number, ri: number) => {
      if (e.button !== 0) return;
      e.preventDefault();
      e.stopPropagation();
      const g = e.currentTarget as HTMLElement;
      const prev = g.previousElementSibling as HTMLElement | null;
      const next = g.nextElementSibling as HTMLElement | null;
      if (!prev || !next) return;
      const r0 = prev.getBoundingClientRect();
      const r1 = next.getBoundingClientRect();
      const size0 = axis === "x" ? r0.width : r0.height;
      const size1 = axis === "x" ? r1.width : r1.height;
      const g0 = axis === "x" ? colGrow(ci) : rowGrow(ci, ri);
      const g1 = axis === "x" ? colGrow(ci + 1) : rowGrow(ci, ri + 1);
      gutterDrag.current = {
        axis, ci, ri,
        start: axis === "x" ? e.clientX : e.clientY,
        size0, size1, sumGrow: g0 + g1,
        cols: columns.map((_, c) => colGrow(c)),
        rows: columns[ci].map((_, r) => rowGrow(ci, r)),
      };
      setGutterActive(true);
    },
    // colGrow/rowGrow close over paneSizes; columns over the card.
    [paneSizes, columns], // eslint-disable-line react-hooks/exhaustive-deps
  );

  useEffect(() => {
    if (!gutterActive) return;
    const MIN = 48; // px — never collapse a pane to nothing
    const onMove = (e: MouseEvent) => {
      const d = gutterDrag.current;
      if (!d) return;
      const pos = d.axis === "x" ? e.clientX : e.clientY;
      const total = d.size0 + d.size1;
      const newSize0 = Math.max(MIN, Math.min(total - MIN, d.size0 + (pos - d.start)));
      const ratio = total > 0 ? newSize0 / total : 0.5;
      const ng0 = ratio * d.sumGrow;
      const ng1 = (1 - ratio) * d.sumGrow;
      if (d.axis === "x") {
        const cols = d.cols.slice();
        cols[d.ci] = ng0; cols[d.ci + 1] = ng1;
        setColSizes(layoutKey, cols);
      } else {
        const rows = d.rows.slice();
        rows[d.ri] = ng0; rows[d.ri + 1] = ng1;
        setRowSizes(layoutKey, d.ci, rows);
      }
    };
    const onUp = () => { gutterDrag.current = null; setGutterActive(false); };
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
    return () => {
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
    };
  }, [gutterActive, layoutKey, setColSizes, setRowSizes]);

  // ── Progressive alert enrichment (info band + daily pane data) ─────────────
  // Kicked off when an alert lands; only runs for strategies whose card declares
  // enrichment/LLM needs, so other strategies just get null here.
  useEffect(() => {
    if (zone.symbol && zone.strategy_id) {
      api.startAlertEnrichment(zone.symbol, zone.strategy_id).catch(() => {});
    }
  }, [zone.symbol, zone.strategy_id]);

  const { data: enrichment } = useQuery({
    queryKey: ["alert_enrichment", zone.symbol],
    queryFn:  () => api.getAlertEnrichment(zone.symbol!),
    enabled:  !!zone.symbol,
    refetchInterval: 700,
  });

  // ── Poll market snapshot for live bid/ask ─────────────────────────────────
  const { data: snapshot } = useQuery({
    queryKey: ["market_snapshot"],
    queryFn:  () => api.getMarketSnapshot(),
    refetchInterval: 500,
    enabled: !!zone.symbol,
  });
  const liveState = zone.symbol ? (snapshot?.tickers[zone.symbol] ?? null) : null;

  // ── Poll open positions to show entry overlay + enable Close button ────────
  const { data: positions } = useQuery({
    queryKey: ["internal_positions"],
    queryFn:  () => api.getInternalPositions(),
    refetchInterval: 1000,
    enabled: !!zone.symbol,
  });
  const openPosition = positions?.find((p) => p.symbol === zone.symbol) ?? null;
  const hasPosition  = openPosition != null;

  // ── Per-symbol info-band extras (score / cap / float / BBZ / PM vol / news) ─
  // Mixes static DB data (score, cap, float, meta) with live-derived fields
  // (Bollinger Z off the live price, premarket volume, news presence), so it's
  // polled rather than cached — the common info bar's metrics stay current.
  const { data: cardInfo } = useQuery({
    queryKey: ["card_info", zone.symbol],
    queryFn:  () => api.getCardInfo(zone.symbol!),
    enabled:  !!zone.symbol,
    refetchInterval: 3000,
  });

  // ── HOD Drive overlay (5 KPIs + HOD/LOD levels + green-series bar times) ─────
  // Recomputed live for the displayed ticker; only fetched for a HOD Drive zone.
  const { data: hodOverlay } = useQuery({
    queryKey: ["hod_drive_overlay", zone.symbol],
    queryFn:  () => api.getHodDriveOverlay(zone.symbol!),
    enabled:  !!zone.symbol && zone.strategy_id === HOD_DRIVE_ID,
    refetchInterval: 3000,
  });

  // ── Recent single-ticker headlines (Alpaca news REST) for the Micro overlay ─
  // Fetched as soon as the ticker is displayed; refreshed periodically to pick up
  // new headlines. The overlay derives the freshness badge from each created_at.
  const { data: tickerNews } = useQuery({
    queryKey: ["ticker_news", zone.symbol],
    queryFn:  () => api.getTickerNews(zone.symbol!),
    enabled:  !!zone.symbol && zone.strategy_id === MICRO_PULLBACK_ID,
    refetchInterval: 60_000,
    staleTime: 30_000,
  });

  // ── Current (forming) bar volume on the displayed timeframe ────────────────
  // Polls the live ring buffer (forming bar included) so the common info bar's
  // "Vol barre" updates as the candle fills.
  const { data: tfBars } = useQuery({
    queryKey: ["tf_bars_vol", zone.symbol, timeframe],
    queryFn:  () => api.getTickerBars(zone.symbol!, timeframe),
    enabled:  !!zone.symbol,
    refetchInterval: 1500,
  });
  const currentBarVolume = tfBars && tfBars.length > 0
    ? tfBars[tfBars.length - 1].volume
    : null;

  // ── Load existing context when zone symbol changes ─────────────────────────
  useEffect(() => {
    if (!zone.symbol) return;
    api.getZoneTradeContext(zone.zone_id, zone.symbol!).then((ctx) => {
      chartStore.setContext(zone.zone_id, ctx ?? null);
    });
  }, [zone.zone_id, zone.symbol]); // eslint-disable-line react-hooks/exhaustive-deps

  // ── Load + keep persisted price alarms in sync for this symbol ─────────────
  // Polled (not one-shot) so a deletion from the Alarms sidebar/panel — which
  // removes the DB row — propagates to the chart and clears its price line.
  const { data: alarmRows } = useQuery({
    queryKey: ["alarms_for_symbol", zone.symbol],
    queryFn:  () => api.getAlarmsForSymbol(zone.symbol!),
    enabled:  !!zone.symbol,
    refetchInterval: 2000,
  });
  useEffect(() => {
    if (!zone.symbol) {
      chartStore.setAlarms(zone.zone_id, []);
      return;
    }
    if (alarmRows) {
      chartStore.setAlarms(zone.zone_id, alarmRows.map((a) => ({ id: a.id, price: a.price })));
    }
  }, [alarmRows, zone.zone_id, zone.symbol]); // eslint-disable-line react-hooks/exhaustive-deps

  // ── Load persisted drawings (lines + text) for this symbol ─────────────────
  // Drawings are memorised per TICKER, so they reappear on any zone/chart showing
  // the symbol and survive restarts. Reloading on symbol change also replaces any
  // stale drawings left from a previous assignment of this zone.
  useEffect(() => {
    if (!zone.symbol) {
      chartStore.setLines(zone.zone_id, []);
      chartStore.setAnnotations(zone.zone_id, []);
      return;
    }
    api.getDrawingsForSymbol(zone.symbol).then((rows) => {
      const lines: ChartLine[] = rows
        .filter((d) => d.kind === "line" && d.t2 != null && d.p2 != null)
        .map((d) => ({
          id: d.id,
          point1: { time: d.t1, price: d.p1 },
          point2: { time: d.t2!, price: d.p2! },
          scope: (d.scope ?? "intraday") as DrawScope,
          color: d.color ?? "#f59e0b",
          opacity: d.opacity ?? 1,
          width: d.width ?? 2,
          lineStyle: (d.line_style ?? "solid") as LineStyleName,
        }));
      const anns: ChartAnnotation[] = rows
        .filter((d) => d.kind === "text" || d.kind === "emoji")
        .map((d) => ({
          id: d.id,
          kind: d.kind === "emoji" ? "emoji" : "text",
          time: d.t1, price: d.p1, text: d.text ?? "",
          scope: (d.scope ?? "intraday") as DrawScope,
          color: d.color ?? (d.kind === "emoji" ? "#ffffff" : "#fcd34d"),
          opacity: d.opacity ?? 1,
          fontSize: d.font_size ?? (d.kind === "emoji" ? 24 : 12),
          pixelX: 0, pixelY: 0,
        }));
      chartStore.setLines(zone.zone_id, lines);
      chartStore.setAnnotations(zone.zone_id, anns);
    }).catch(() => {});
  }, [zone.zone_id, zone.symbol]); // eslint-disable-line react-hooks/exhaustive-deps

  // ── Seed the interactive pane's timeframe from the strategy card when a new
  //    alert lands in this zone. Re-seeds only on a new alert_id, so the user's
  //    manual timeframe changes persist for the current alert. ────────────────
  const seededAlertRef = useRef<string | null>(null);
  useEffect(() => {
    // Per-alert timeframe override (e.g. Perfect Pullback's triggering timeframe)
    // wins over the card's static pane timeframe.
    const tf0 = (zone.display_timeframe as Timeframe | null) ?? card?.panes?.[interactiveIdx]?.timeframe;
    if (tf0 && zone.alert_id && seededAlertRef.current !== zone.alert_id) {
      chartStore.setTimeframe(zone.zone_id, tf0 as Timeframe);
      seededAlertRef.current = zone.alert_id;
    }
  }, [card, interactiveIdx, zone.alert_id, zone.zone_id, zone.display_timeframe]); // eslint-disable-line react-hooks/exhaustive-deps

  // ── On position close, refetch the context. The backend clears the zone's
  //    SL/TP when a trade goes flat (so the bracket lines disappear) but KEEPS
  //    the tradeID — the journal/screenshots stay available for the closed trade
  //    until a new SL/TP is placed, which then starts a fresh trade. ──────────
  const prevHasPosition = useRef(false);
  useEffect(() => {
    if (prevHasPosition.current && !hasPosition && zone.symbol) {
      api.getZoneTradeContext(zone.zone_id, zone.symbol!).then((ctx) => {
        chartStore.setContext(zone.zone_id, ctx ?? null);
      });
    }
    prevHasPosition.current = hasPosition;
  }, [hasPosition, zone.zone_id, zone.symbol]); // eslint-disable-line react-hooks/exhaustive-deps

  // ── Detect narrow toolbar ─────────────────────────────────────────────────
  // Compact (collapse secondary tools into an overflow menu) only when the full
  // toolbar genuinely overflows the header — not at an arbitrary width. The
  // header's flex-1 spacer shrinks to 0 first, so scrollWidth > clientWidth is a
  // true "doesn't fit" signal. Hysteresis (re-expand only once we have clearly
  // more room than where we compacted) prevents flip-flopping at the boundary.
  const compactAtWidth = useRef(0);
  useEffect(() => {
    const el = headerRef.current;
    if (!el) return;
    const ro = new ResizeObserver(() => {
      setNarrowToolbar((prev) => {
        if (!prev) {
          if (el.scrollWidth > el.clientWidth + 1) {
            compactAtWidth.current = el.clientWidth;
            return true;
          }
          return false;
        }
        // Currently compact → expand when there's clearly room again.
        return el.clientWidth <= compactAtWidth.current + 80;
      });
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  // ── Release zone ──────────────────────────────────────────────────────────
  const handleRelease = useCallback(() => {
    // local cleanup first — don't gate on API
    chartStore.clearZone(zone.zone_id);
    releaseZone(zone.zone_id);
    api.clearZoneContext(zone.zone_id).catch(() => {});
  }, [zone.zone_id, releaseZone, chartStore]);

  // ── Chart click (mode dispatch) ────────────────────────────────────────────
  const handlePriceClick = useCallback(async (
    price: number,
    time:  number,
    _pixelX: number,
    _pixelY: number,
    scope: DrawScope,
  ) => {
    // Read drawMode and linePoint1 from the store at call time — avoids any
    // stale-closure issue where the captured value lags behind the click.
    const zs   = useChartStore.getState().getZone(zone.zone_id);
    const mode = zs.drawMode;
    const lp1  = zs.linePoint1;

    const sym   = zone.symbol;
    const strat = zone.strategy_id ?? "";

    if (mode === "sl" || mode === "tp") {
      if (!sym) return;
      try {
        const ctx = mode === "sl"
          ? await api.updateZoneSl(zone.zone_id, sym, strat, price)
          : await api.updateZoneTp(zone.zone_id, sym, strat, price);
        chartStore.setContext(zone.zone_id, ctx);
      } catch (e) {
        console.error(`update_zone_${mode} failed:`, e);
      }
      chartStore.setDrawMode(zone.zone_id, "none");
    } else if (mode === "alarm") {
      if (!sym) return;
      // Persist the alarm (id · price · ticker · strategy · created_at) then
      // draw its price line. Triggering is wired in a later iteration.
      const id = (crypto?.randomUUID?.() ?? `alarm-${Date.now()}`);
      try {
        const saved = await api.createAlarm(id, sym, strat || null, price);
        chartStore.addAlarm(zone.zone_id, { id: saved.id, price: saved.price });
      } catch (e) {
        console.error("create_alarm failed:", e);
      }
      chartStore.setDrawMode(zone.zone_id, "none");
    } else if (mode === "line") {
      if (!lp1) {
        chartStore.setLinePoint1(zone.zone_id, { time, price });
      } else {
        const sty = useDrawingPrefs.getState().line;
        const line: ChartLine = {
          id: `line-${Date.now()}`, point1: lp1, point2: { time, price },
          scope, color: sty.color, opacity: sty.opacity, width: sty.width, lineStyle: sty.lineStyle,
        };
        chartStore.addLine(zone.zone_id, line);
        chartStore.setLinePoint1(zone.zone_id, null);
        chartStore.setDrawMode(zone.zone_id, "none");
        // Memorise on the ticker (persisted, shown on every chart of this symbol).
        if (sym) {
          api.createDrawing({
            id: line.id, symbol: sym, kind: "line",
            t1: lp1.time, p1: lp1.price, t2: time, p2: price, text: null,
            scope, color: line.color, opacity: line.opacity, width: line.width, line_style: line.lineStyle,
          }).catch(() => {});
        }
      }
    }
  }, [zone.zone_id, zone.symbol, zone.strategy_id, chartStore]);

  // ── SL/TP drag end ─────────────────────────────────────────────────────────
  const handleSlDragEnd = useCallback(async (price: number) => {
    const sym = zone.symbol;
    if (!sym) return;
    const strat = zone.strategy_id ?? "";
    try {
      const ctx = await api.updateZoneSl(zone.zone_id, sym, strat, price);
      chartStore.setContext(zone.zone_id, ctx);
    } catch (e) { console.error("updateZoneSl drag failed:", e); }
  }, [zone.zone_id, zone.symbol, zone.strategy_id, chartStore]);

  const handleTpDragEnd = useCallback(async (price: number) => {
    const sym = zone.symbol;
    if (!sym) return;
    const strat = zone.strategy_id ?? "";
    try {
      const ctx = await api.updateZoneTp(zone.zone_id, sym, strat, price);
      chartStore.setContext(zone.zone_id, ctx);
    } catch (e) { console.error("updateZoneTp drag failed:", e); }
  }, [zone.zone_id, zone.symbol, zone.strategy_id, chartStore]);

  // ── Delete a user price line from its on-chart ✕ ───────────────────────────
  // SL/TP clear by setting the level to null (also drops the bracket order);
  // an alarm clears by deleting its persisted row (the poll keeps the chart in
  // sync, and removeAlarm drops the line immediately).
  const handleDeleteSl = useCallback(async () => {
    const sym = zone.symbol;
    if (!sym) return;
    const strat = zone.strategy_id ?? "";
    try {
      const ctx = await api.updateZoneSl(zone.zone_id, sym, strat, null);
      chartStore.setContext(zone.zone_id, ctx);
    } catch (e) { console.error("delete SL failed:", e); }
  }, [zone.zone_id, zone.symbol, zone.strategy_id, chartStore]);

  const handleDeleteTp = useCallback(async () => {
    const sym = zone.symbol;
    if (!sym) return;
    const strat = zone.strategy_id ?? "";
    try {
      const ctx = await api.updateZoneTp(zone.zone_id, sym, strat, null);
      chartStore.setContext(zone.zone_id, ctx);
    } catch (e) { console.error("delete TP failed:", e); }
  }, [zone.zone_id, zone.symbol, zone.strategy_id, chartStore]);

  const handleDeleteAlarm = useCallback(async (id: string) => {
    chartStore.removeAlarm(zone.zone_id, id);
    try { await api.deleteAlarm(id); } catch (e) { console.error("delete alarm failed:", e); }
  }, [zone.zone_id, chartStore]);

  // ── Create a text / emoji annotation (scope = the pane it was placed on) ───
  const handleCreateAnnotation = useCallback((
    kind: "text" | "emoji", time: number, price: number, text: string, scope: DrawScope,
  ) => {
    const prefs = useDrawingPrefs.getState();
    const ann: ChartAnnotation = {
      id: `ann-${Date.now()}`, kind, time, price, text, scope,
      color:    kind === "emoji" ? "#ffffff" : prefs.text.color,
      opacity:  kind === "emoji" ? 1 : prefs.text.opacity,
      fontSize: kind === "emoji" ? prefs.emoji.fontSize : prefs.text.fontSize,
      pixelX: 0, pixelY: 0,
    };
    chartStore.addAnnotation(zone.zone_id, ann);
    chartStore.setDrawMode(zone.zone_id, "none");
    if (zone.symbol) {
      api.createDrawing({
        id: ann.id, symbol: zone.symbol, kind,
        t1: time, p1: price, t2: null, p2: null, text,
        scope, color: ann.color, opacity: ann.opacity, font_size: ann.fontSize,
      }).catch(() => {});
    }
  }, [zone.zone_id, zone.symbol, chartStore]);

  // Persist a drawing change to the DB (style/position). Builds the row from the
  // current store state of `id` (line or annotation).
  const persistDrawing = useCallback((id: string) => {
    const sym = zone.symbol;
    if (!sym) return;
    const z = useChartStore.getState().getZone(zone.zone_id);
    const l = z.lines.find((x) => x.id === id);
    if (l) {
      api.updateDrawing({
        id, symbol: sym, kind: "line",
        t1: l.point1.time, p1: l.point1.price, t2: l.point2.time, p2: l.point2.price, text: null,
        scope: l.scope, color: l.color, opacity: l.opacity, width: l.width, line_style: l.lineStyle,
      }).catch(() => {});
      return;
    }
    const a = z.annotations.find((x) => x.id === id);
    if (a) {
      api.updateDrawing({
        id, symbol: sym, kind: a.kind,
        t1: a.time, p1: a.price, t2: null, p2: null, text: a.text,
        scope: a.scope, color: a.color, opacity: a.opacity, font_size: a.fontSize,
      }).catch(() => {});
    }
  }, [zone.zone_id, zone.symbol]);

  const handleLineChange = useCallback((id: string, point1: { time: number; price: number }, point2: { time: number; price: number }) => {
    chartStore.updateLine(zone.zone_id, id, { point1, point2 });
    persistDrawing(id);
  }, [zone.zone_id, chartStore, persistDrawing]);

  const handleAnnotationMove = useCallback((id: string, time: number, price: number) => {
    chartStore.updateAnnotation(zone.zone_id, id, { time, price });
    persistDrawing(id);
  }, [zone.zone_id, chartStore, persistDrawing]);

  const handleEditAnnotation = useCallback((id: string, text: string) => {
    chartStore.updateAnnotation(zone.zone_id, id, { text });
    persistDrawing(id);
  }, [zone.zone_id, chartStore, persistDrawing]);

  const handleStyleLine = useCallback((id: string, patch: Partial<ChartLine>) => {
    chartStore.updateLine(zone.zone_id, id, patch);
    // Last-used style becomes the default for the next line.
    useDrawingPrefs.getState().setLine(patch as Partial<ReturnType<typeof useDrawingPrefs.getState>["line"]>);
    persistDrawing(id);
  }, [zone.zone_id, chartStore, persistDrawing]);

  const handleStyleAnnotation = useCallback((id: string, patch: Partial<ChartAnnotation>) => {
    chartStore.updateAnnotation(zone.zone_id, id, patch);
    const z = useChartStore.getState().getZone(zone.zone_id);
    const a = z.annotations.find((x) => x.id === id);
    if (a?.kind === "emoji") { if (patch.fontSize != null) useDrawingPrefs.getState().setEmoji({ fontSize: patch.fontSize }); }
    else useDrawingPrefs.getState().setText(patch as Partial<ReturnType<typeof useDrawingPrefs.getState>["text"]>);
    persistDrawing(id);
  }, [zone.zone_id, chartStore, persistDrawing]);

  const handleDeleteLine = useCallback((id: string) => {
    chartStore.removeLine(zone.zone_id, id);
    api.deleteDrawing(id).catch(() => {});
  }, [zone.zone_id, chartStore]);

  const handleDeleteAnnotation = useCallback((id: string) => {
    chartStore.removeAnnotation(zone.zone_id, id);
    api.deleteDrawing(id).catch(() => {});
  }, [zone.zone_id, chartStore]);

  const handleDuplicateLine = useCallback((id: string) => {
    const z = useChartStore.getState().getZone(zone.zone_id);
    const l = z.lines.find((x) => x.id === id);
    if (!l || !zone.symbol) return;
    // Offset the copy slightly in price so it's visible.
    const dp = (l.point1.price + l.point2.price) * 0.01;
    const copy: ChartLine = {
      ...l, id: `line-${Date.now()}`,
      point1: { ...l.point1, price: l.point1.price + dp },
      point2: { ...l.point2, price: l.point2.price + dp },
    };
    chartStore.addLine(zone.zone_id, copy);
    api.createDrawing({
      id: copy.id, symbol: zone.symbol, kind: "line",
      t1: copy.point1.time, p1: copy.point1.price, t2: copy.point2.time, p2: copy.point2.price, text: null,
      scope: copy.scope, color: copy.color, opacity: copy.opacity, width: copy.width, line_style: copy.lineStyle,
    }).catch(() => {});
  }, [zone.zone_id, zone.symbol, chartStore]);

  const handleAlarmDragEnd = useCallback((id: string, price: number) => {
    const z = useChartStore.getState().getZone(zone.zone_id);
    chartStore.setAlarms(zone.zone_id, z.alarms.map((a) => a.id === id ? { ...a, price } : a));
    api.updateAlarmPrice(id, price).catch(() => {});
  }, [zone.zone_id, chartStore]);

  const handleContextMenu = useCallback((target: CtxTarget, clientX: number, clientY: number) => {
    setCtxMenu({ target, x: clientX, y: clientY });
  }, []);

  // ── Toggle draw mode (click same button to deactivate) ────────────────────
  const toggleMode = useCallback((mode: DrawMode) => {
    chartStore.setDrawMode(zone.zone_id, drawMode === mode ? "none" : mode);
  }, [drawMode, zone.zone_id, chartStore]);

  // ── Place internal order (25 / 50 / 100 %) ────────────────────────────────
  const handleOrder = useCallback(async (percent: 25 | 50 | 100) => {
    if (!hasSl) return;
    try {
      if (orderMode === "market") {
        await api.createInternalMarketOrderPercent(zone.zone_id, percent);
      } else {
        await api.createInternalOrderPercent(zone.zone_id, percent);
      }
    } catch (e) {
      console.error("order failed:", e);
    }
  }, [zone.zone_id, hasSl, orderMode]);

  // ── Close open position ────────────────────────────────────────────────────
  const handleClose = useCallback(async () => {
    if (!zone.symbol || !hasPosition) return;
    try {
      await api.closeInternalPosition(zone.symbol, zone.zone_id);
    } catch (e) {
      console.error("close position failed:", e);
    }
  }, [zone.symbol, zone.zone_id, hasPosition]);

  // ── Screenshot capture ────────────────────────────────────────────────────
  const handleCapture = useCallback(async () => {
    const container = chartAreaRef.current;
    if (!container) return;

    setCaptureStatus("pending");
    try {
      // Build the info-band header text (strategy + chips + reason + LLM). Each
      // top-level child of the band becomes one line; its internal chips are
      // joined with " · ". Canvas text can't be tainted, so this is safe.
      const PAD = 8;
      const LINE_H = 15;
      const FONT = "11px ui-monospace, monospace";
      const headerLines: string[] = [];
      const band = infoBandRef.current;
      if (band) {
        band.childNodes.forEach((node) => {
          if (node.nodeType !== Node.ELEMENT_NODE) return;
          const txt = ((node as HTMLElement).innerText || "")
            .replace(/\s*\n\s*/g, " · ")
            .replace(/\s{2,}/g, " ")
            .trim();
          if (txt) headerLines.push(txt);
        });
      }

      const rect = container.getBoundingClientRect();
      const width = Math.round(rect.width);

      // Wrap header lines to the canvas width so long LLM summaries don't clip.
      const measure = document.createElement("canvas").getContext("2d")!;
      measure.font = FONT;
      const wrapped: string[] = [];
      const maxTextW = width - PAD * 2;
      for (const line of headerLines) {
        const words = line.split(" ");
        let cur = "";
        for (const w of words) {
          const next = cur ? `${cur} ${w}` : w;
          if (measure.measureText(next).width > maxTextW && cur) {
            wrapped.push(cur);
            cur = w;
          } else {
            cur = next;
          }
        }
        if (cur) wrapped.push(cur);
      }
      const headerH = wrapped.length > 0 ? wrapped.length * LINE_H + PAD * 2 : 0;

      // Composite the info header (text) + all chart canvases onto one canvas.
      const canvas = document.createElement("canvas");
      canvas.width  = width;
      canvas.height = Math.round(rect.height) + headerH;
      const ctx = canvas.getContext("2d");
      if (!ctx) throw new Error("no 2d context");

      ctx.fillStyle = "#0a0a0a";
      ctx.fillRect(0, 0, canvas.width, canvas.height);

      if (headerH > 0) {
        ctx.font = FONT;
        ctx.textBaseline = "top";
        ctx.fillStyle = "#cfcfcf";
        wrapped.forEach((line, i) => {
          ctx.fillText(line, PAD, PAD + i * LINE_H);
        });
        // Thin separator between the info band and the chart.
        ctx.fillStyle = "#1e1e1e";
        ctx.fillRect(0, headerH - 1, canvas.width, 1);
      }

      const srcCanvases = container.querySelectorAll("canvas");
      srcCanvases.forEach((src) => {
        const sr = src.getBoundingClientRect();
        ctx.drawImage(src, sr.left - rect.left, sr.top - rect.top + headerH);
      });

      // The on-chart strategy overlay is a DOM layer (not a canvas), so rasterise
      // its cells onto the composite by hand — at their on-chart position, below
      // the info header — so screenshots show the strategy-specific fields too.
      const overlayCells = container.querySelectorAll<HTMLElement>("[data-capture-cell]");
      overlayCells.forEach((cell) => {
        const cr = cell.getBoundingClientRect();
        const x = cr.left - rect.left;
        const y = cr.top - rect.top + headerH;
        const w = cr.width;
        const h = cr.height;
        const padX = 6;
        // Opaque box (the live backdrop-blur can't be reproduced on canvas).
        ctx.fillStyle = "rgba(0,0,0,0.55)";
        ctx.fillRect(x, y, w, h);
        // Prefer explicit data-cap-label / data-cap-value (rich Micro overlay,
        // where a bar sits between them); fall back to the generic overlay's
        // first two children (label over value).
        const labelEl = cell.querySelector<HTMLElement>("[data-cap-label]") ?? cell.children[0];
        const valueEl = cell.querySelector<HTMLElement>("[data-cap-value]") ?? cell.children[1];
        const label = labelEl?.textContent?.trim() ?? "";
        const value = valueEl?.textContent?.trim() ?? "";
        if (label) {
          ctx.font = "9px ui-monospace, monospace";
          ctx.textBaseline = "top";
          ctx.fillStyle = "#9a9a9a";
          ctx.fillText(label, x + padX, y + 3);
        }
        if (value) {
          ctx.font = "bold 14px ui-monospace, monospace";
          ctx.textBaseline = "bottom";
          ctx.fillStyle = "#e5e5e5";
          ctx.fillText(value, x + padX, y + h - 3);
        }
      });

      const dataUrl = canvas.toDataURL("image/png");

      const ts  = nyFilenameStamp(); // New York time
      const filename = hasTradeId
        ? `${context!.trade_id}_${ts}.png`
        : `capture_${ts}.png`;

      if (hasTradeId) {
        await api.saveScreenshotLocal(
          zone.zone_id,
          context!.trade_id ?? null,
          dataUrl,
          filename,
        );
        setCaptureStatus("success");
        setTimeout(() => setCaptureStatus("idle"), 3000);
      } else {
        // No tradeID — trigger browser download
        const a = document.createElement("a");
        a.href     = dataUrl;
        a.download = filename;
        a.click();
        setCaptureStatus("success");
        setTimeout(() => setCaptureStatus("idle"), 2000);
      }
    } catch (e) {
      console.error("capture failed:", e);
      setCaptureStatus("error");
      setTimeout(() => setCaptureStatus("idle"), 3000);
    }
  }, [zone.zone_id, hasTradeId, context]);

  // ── Controller (gamepad) zone handlers ─────────────────────────────────────
  // R1 cycles the focused pane; the cursor-layer buttons (A/B/Y, R2 up) drop
  // SL/TP/alarm at the horizontal cursor's price — read live from the focused
  // pane's chart control, seeded at the current price if never moved. X clears the
  // ticker's resting orders + planned SL/TP (double-tap also clears its alarms).
  const cycleFocus = useCallback(() => {
    setFocusPaneIdx((i) => {
      if (panes.length <= 1) return i;
      // Drop the cursor on the pane losing focus so only the focused pane shows it.
      getChartControl(`${zone.zone_id}-${i}`)?.clearCursor();
      return (i + 1) % panes.length;
    });
  }, [panes.length, zone.zone_id]);

  const cursorPlace = useCallback(async (kind: "sl" | "tp" | "alarm") => {
    const sym = zone.symbol;
    if (!sym) return;
    const ctl = getChartControl(`${zone.zone_id}-${focusPaneIdxRef.current}`);
    if (!ctl) return;
    let price = ctl.getCursorPrice();
    if (price == null) { ctl.nudgeCursor(0); price = ctl.getCursorPrice(); } // seed at current price
    if (price == null) return;
    const strat = zone.strategy_id ?? "";
    try {
      if (kind === "sl") {
        chartStore.setContext(zone.zone_id, await api.updateZoneSl(zone.zone_id, sym, strat, price));
      } else if (kind === "tp") {
        chartStore.setContext(zone.zone_id, await api.updateZoneTp(zone.zone_id, sym, strat, price));
      } else {
        const id = crypto?.randomUUID?.() ?? `alarm-${Date.now()}`;
        const saved = await api.createAlarm(id, sym, strat || null, price);
        chartStore.addAlarm(zone.zone_id, { id: saved.id, price: saved.price });
      }
    } catch (e) { console.error(`cursor ${kind} failed:`, e); }
  }, [zone.zone_id, zone.symbol, zone.strategy_id, chartStore]);

  const removeOrders = useCallback(async () => {
    const sym = zone.symbol;
    if (!sym) return;
    const strat = zone.strategy_id ?? "";
    try {
      const orders = await api.getInternalOrders();
      await Promise.all(
        orders.filter((o) => o.symbol === sym).map((o) => api.cancelInternalOrder(o.order_id)),
      );
      await api.updateZoneSl(zone.zone_id, sym, strat, null);
      chartStore.setContext(zone.zone_id, await api.updateZoneTp(zone.zone_id, sym, strat, null));
    } catch (e) { console.error("remove orders failed:", e); }
  }, [zone.zone_id, zone.symbol, zone.strategy_id, chartStore]);

  const removeOrdersAndAlarms = useCallback(async () => {
    await removeOrders();
    const sym = zone.symbol;
    if (!sym) return;
    try {
      const al = await api.getAlarmsForSymbol(sym);
      await Promise.all(al.map((a) => api.deleteAlarm(a.id)));
      chartStore.setAlarms(zone.zone_id, []);
    } catch (e) { console.error("remove alarms failed:", e); }
  }, [removeOrders, zone.zone_id, zone.symbol, chartStore]);

  // ── Hotkeys: run a bindable action on this zone ────────────────────────────
  // Mirrors the toolbar buttons / timeframe dropdown / IA button. The global
  // listener (useHotkeys) routes a chord to the hovered zone's runner; timeframe
  // actions drive the interactive (left) pane via the same setTimeframe the
  // dropdown uses.
  const runAction = useCallback((id: HotkeyActionId) => {
    switch (id) {
      case "release":    handleRelease(); break;
      case "sl":         toggleMode("sl"); break;
      case "tp":         toggleMode("tp"); break;
      case "alarm":      toggleMode("alarm"); break;
      case "line":       toggleMode("line"); break;
      case "text":       toggleMode("text"); break;
      case "capture":    if (captureStatus === "idle") handleCapture(); break;
      case "journal":    openJournal(); break;
      case "order_mode": chartStore.setOrderMode(zone.zone_id, orderMode === "market" ? "limit" : "market"); break;
      case "order_25":   handleOrder(25); break;
      case "order_50":   handleOrder(50); break;
      case "order_100":  handleOrder(100); break;
      case "close":      handleClose(); break;
      case "run_llm":
        if (card?.llm && zone.symbol && zone.strategy_id) {
          api.runAlertLlm(zone.symbol, zone.strategy_id).catch(() => {});
        }
        break;
      default: {
        const tf = TF_FOR_ACTION[id];
        if (tf) chartStore.setTimeframe(zone.zone_id, tf);
      }
    }
  }, [handleRelease, toggleMode, captureStatus, handleCapture, openJournal,
      chartStore, zone.zone_id, zone.symbol, zone.strategy_id, orderMode,
      handleOrder, handleClose, card]);

  useEffect(() => registerZoneHotkeys(zone.zone_id, runAction), [zone.zone_id, runAction]);

  // ── Gamepad: register this zone's controller handlers ──────────────────────
  // The global gamepad loop (useGamepad) routes button presses + stick input to
  // the active session's zone through this registry. Registered only while the
  // zone shows a ticker. tradeID / symbol are read live from the store on demand.
  useEffect(() => {
    if (!zone.symbol) return;
    return registerZoneGamepad(zone.zone_id, {
      getFocusedPaneId: () => `${zone.zone_id}-${focusPaneIdxRef.current}`,
      cycleFocus,
      placeSl:    () => cursorPlace("sl"),
      placeTp:    () => cursorPlace("tp"),
      placeAlarm: () => cursorPlace("alarm"),
      removeOrders,
      removeOrdersAndAlarms,
      order:   (pct) => handleOrder(pct),
      close:   handleClose,
      capture: () => { if (captureStatus === "idle") handleCapture(); },
      journalAudio: toggleAudioNote,
      release: handleRelease,
      hasTradeId: () => !!useChartStore.getState().getZone(zone.zone_id).context?.trade_id,
      tradeId:    () => useChartStore.getState().getZone(zone.zone_id).context?.trade_id ?? null,
      symbol:     () => zone.symbol,
    });
  }, [zone.zone_id, zone.symbol, cycleFocus, cursorPlace, removeOrders,
      removeOrdersAndAlarms, handleOrder, handleClose, handleCapture, captureStatus, handleRelease, toggleAudioNote]);

  // ── Empty zone ─────────────────────────────────────────────────────────────
  if (!zone.symbol) {
    return <EmptyZone zone={zone} />;
  }

  const styles = zone.priority != null ? PRIORITY_STYLES[zone.priority] ?? null : null;

  // ── Toolbar button definitions ────────────────────────────────────────────

  const tbRelease = (
    <TBtn
      key="release"
      title="Libérer la zone"
      onClick={handleRelease}
      className="text-rose-400 hover:bg-rose-900/30 hover:text-rose-300"
    >
      <Bird className="h-3 w-3" />
      {!narrowToolbar && <span>Libérer</span>}
    </TBtn>
  );

  const tbLine = (
    <TBtn key="line" title="Mode ligne" active={drawMode === "line"}
      onClick={() => toggleMode("line")}>
      <Slash className="h-3 w-3" />
    </TBtn>
  );

  const tbText = (
    <TBtn key="text" title="Mode texte" active={drawMode === "text"}
      onClick={() => toggleMode("text")}>
      <Type className="h-3 w-3" />
    </TBtn>
  );

  const tbEmoji = (
    <DropdownMenu key="emoji">
      <DropdownMenuTrigger asChild>
        <button
          title="Ajouter un emoji d'humeur"
          className={cn(
            "flex h-5 shrink-0 items-center gap-0.5 rounded px-1.5 text-[10px] font-medium transition-colors",
            drawMode === "emoji" ? "bg-accent text-foreground" : "text-muted-foreground hover:bg-accent hover:text-foreground",
          )}
        >
          <Smile className="h-3 w-3" />
        </button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="start" className="min-w-0">
        <div className="flex gap-0.5 p-1">
          {EMOJIS.map((e) => (
            <button
              key={e.glyph}
              title={e.title}
              onClick={() => {
                chartStore.setDrawMode(zone.zone_id, "emoji");
                chartStore.setPendingEmoji(zone.zone_id, e.glyph);
              }}
              className={cn(
                "rounded px-1.5 py-1 text-lg leading-none hover:bg-accent",
                pendingEmoji === e.glyph && drawMode === "emoji" && "bg-accent",
              )}
            >
              {e.glyph}
            </button>
          ))}
        </div>
      </DropdownMenuContent>
    </DropdownMenu>
  );

  const tbClock = (
    <DropdownMenu key="clock">
      <DropdownMenuTrigger asChild>
        <button
          title="Timeframe"
          className="flex h-5 shrink-0 items-center gap-0.5 rounded px-1.5 text-[10px] font-medium text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
        >
          <Clock className="h-3 w-3" />
          <span>{timeframe}</span>
          <ChevronDown className="h-2.5 w-2.5 opacity-60" />
        </button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="start" className="min-w-[6rem]">
        {TIMEFRAMES.map((tf) => (
          <DropdownMenuItem
            key={tf}
            onClick={() => chartStore.setTimeframe(zone.zone_id, tf)}
            className={cn("text-xs", tf === timeframe && "text-blue-400")}
          >
            {tf}
          </DropdownMenuItem>
        ))}
      </DropdownMenuContent>
    </DropdownMenu>
  );

  const tbSl = (
    <TBtn key="sl" title="Placer Stop Loss" active={drawMode === "sl"}
      onClick={() => toggleMode("sl")}
      className={hasSl ? "text-red-400" : undefined}>
      <span>SL</span>
    </TBtn>
  );

  const tbTp = (
    <TBtn key="tp" title="Placer Take Profit" active={drawMode === "tp"}
      onClick={() => toggleMode("tp")}
      className={hasTp ? "text-green-400" : undefined}>
      <span>TP</span>
    </TBtn>
  );

  const tbAlarm = (
    <TBtn key="alarm" title="Placer une alarme de prix" active={drawMode === "alarm"}
      onClick={() => toggleMode("alarm")}
      className={alarms.length > 0 ? "text-amber-400" : undefined}>
      <AlarmClockPlus className="h-3 w-3" />
    </TBtn>
  );

  const tbSize25 = (
    <TBtn key="25"
      title={hasSl ? `25 % — mode ${orderMode === "market" ? "Mkt" : "Lmt"}` : "SL requis"}
      disabled={!hasSl}
      onClick={() => handleOrder(25)}
    >
      <span>25</span>
    </TBtn>
  );
  const tbSize50 = (
    <TBtn key="50"
      title={hasSl ? `50 % — mode ${orderMode === "market" ? "Mkt" : "Lmt"}` : "SL requis"}
      disabled={!hasSl}
      onClick={() => handleOrder(50)}
    >
      <span>50</span>
    </TBtn>
  );
  const tbSize100 = (
    <TBtn key="100"
      title={hasSl ? `100 % — mode ${orderMode === "market" ? "Mkt" : "Lmt"}` : "SL requis"}
      disabled={!hasSl}
      onClick={() => handleOrder(100)}
    >
      <span>100</span>
    </TBtn>
  );

  const tbOrderMode = (
    <TBtn key="ordermode"
      title={`Mode ordre: ${orderMode === "market" ? "Market" : "Limit"}`}
      onClick={() => chartStore.setOrderMode(zone.zone_id, orderMode === "market" ? "limit" : "market")}
      active={orderMode === "limit"}
    >
      <span>{orderMode === "market" ? "Mkt" : "Lmt"}</span>
    </TBtn>
  );

  const tbClose = (
    <TBtn key="close"
      title={hasPosition ? "Clôturer la position" : "Aucune position ouverte"}
      disabled={!hasPosition}
      onClick={handleClose}
      className={hasPosition ? "text-orange-400 hover:bg-orange-900/30 hover:text-orange-300" : undefined}
    >
      <CircleX className="h-3 w-3" />
    </TBtn>
  );

  const tbCapture = (
    <TBtn
      key="capture"
      title={
        captureStatus === "pending" ? "Capture en cours…"
        : captureStatus === "success" ? "Capturé !"
        : captureStatus === "error" ? "Erreur capture"
        : hasTradeId ? "Capturer (lié au trade)"
        : "Capturer (enregistrer sous)"
      }
      onClick={captureStatus === "idle" ? handleCapture : undefined}
      className={
        captureStatus === "success" ? "text-emerald-400"
        : captureStatus === "error" ? "text-red-400"
        : undefined
      }
    >
      {captureStatus === "pending" ? (
        <LoaderCircle className="h-3 w-3 animate-spin" />
      ) : captureStatus === "success" ? (
        <CheckCircle className="h-3 w-3" />
      ) : (
        <Camera className="h-3 w-3" />
      )}
    </TBtn>
  );

  const tbJournal = (
    <TBtn
      key="journal"
      title={hasTradeId ? "Journal" : "TradeID requis (placer SL ou TP d'abord)"}
      disabled={!hasTradeId}
      onClick={openJournal}
    >
      <NotebookPen className="h-3 w-3" />
    </TBtn>
  );

  const tbAudioNote = (
    <MicDictate
      key="audio-note"
      variant="toolbar"
      mode="trade"
      tradeId={context?.trade_id}
      symbol={zone.symbol}
      disabled={!hasTradeId}
      title={hasTradeId ? "Dicter une note (micro)" : "TradeID requis (placer SL ou TP d'abord)"}
      onRegisterToggle={(t) => { audioToggleRef.current = t; }}
    />
  );

  // Primary = always visible; secondary = in overflow when narrow
  const primaryButtons  = [tbRelease, tbSl, tbTp, tbAlarm, tbClock];
  const secondaryButtons = [tbLine, tbText, tbEmoji, tbSize25, tbSize50, tbSize100, tbOrderMode, tbClose, tbCapture, tbJournal, tbAudioNote];

  // ── Render ─────────────────────────────────────────────────────────────────

  return (
    <div
      data-zone-id={zone.zone_id}
      className={cn(
        "relative flex h-full w-full flex-col overflow-hidden rounded-md border border-border bg-card transition-colors",
        styles?.accent,
      )}
      onMouseEnter={() => setHoveredZone(zone.zone_id)}
    >
      {/* ── Header row: symbol + toolbar ─────────────────────────────────── */}
      <div
        ref={headerRef}
        className="flex min-w-0 items-center gap-1 border-b border-border bg-card/80 px-1.5 py-0.5"
      >
        {/* Symbol + priority */}
        <span className="shrink-0 text-sm font-bold tabular-nums tracking-tight">
          {zone.symbol}
        </span>
        {zone.priority != null && styles && (
          <span className={cn("shrink-0 rounded px-1 py-0.5 text-[9px] font-bold uppercase", styles.badge)}>
            P{zone.priority}
          </span>
        )}

        {/* Timeframe + side badge (e.g. Perfect Pullback timeframe × long/short) */}
        {zone.display_timeframe && (
          <span className="shrink-0 rounded bg-violet-900/50 px-1 py-0.5 text-[9px] font-bold uppercase text-violet-300">
            {zone.display_timeframe}
          </span>
        )}
        {zone.side && (
          <span className={cn(
            "shrink-0 rounded px-1 py-0.5 text-[9px] font-bold uppercase",
            zone.side === "long" ? "bg-emerald-900/50 text-emerald-300" : "bg-red-900/50 text-red-300",
          )}>
            {zone.side}
          </span>
        )}

        {/* LLM badge */}
        {zone.llm_status === "loading" && (
          <span className="flex shrink-0 items-center gap-0.5 rounded bg-blue-900/30 px-1 py-0.5 text-[9px] text-blue-400">
            <LoaderCircle className="h-2.5 w-2.5 animate-spin" />
            LLM
          </span>
        )}
        {zone.llm_status === "error" && (
          <span className="shrink-0 rounded bg-red-900/30 px-1 py-0.5 text-[9px] text-red-400">
            LLM err
          </span>
        )}

        {/* Position chip */}
        {openPosition && (
          <span className={cn(
            "shrink-0 rounded px-1 py-0.5 text-[9px] font-semibold tabular-nums",
            openPosition.side === "long"
              ? "bg-emerald-900/50 text-emerald-400"
              : "bg-red-900/50 text-red-400"
          )}>
            {openPosition.side === "long" ? "+" : ""}{openPosition.quantity}
            {openPosition.unrealized_pnl != null && (
              <span className={openPosition.unrealized_pnl >= 0 ? " text-emerald-300" : " text-red-300"}>
                {" "}{openPosition.unrealized_pnl >= 0 ? "+" : ""}
                ${openPosition.unrealized_pnl.toFixed(2)}
              </span>
            )}
          </span>
        )}

        <div className="flex-1" />

        {/* Primary toolbar buttons */}
        <div className="flex shrink-0 items-center gap-0.5">
          {primaryButtons}

          {/* Secondary: either inline or overflow */}
          {!narrowToolbar ? (
            <>
              <Sep />
              {secondaryButtons}
            </>
          ) : (
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <button
                  title="Plus d'outils"
                  className="flex h-5 items-center rounded px-1 text-muted-foreground hover:bg-accent hover:text-foreground"
                >
                  <MoreHorizontal className="h-3.5 w-3.5" />
                </button>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="end" className="min-w-[9rem]">
                <DropdownMenuItem
                  onClick={() => toggleMode("line")}
                  className={cn("gap-2 text-xs", drawMode === "line" && "text-amber-400")}
                >
                  <Slash className="h-3 w-3" /> Ligne
                </DropdownMenuItem>
                <DropdownMenuItem
                  onClick={() => toggleMode("text")}
                  className={cn("gap-2 text-xs", drawMode === "text" && "text-amber-400")}
                >
                  <Type className="h-3 w-3" /> Texte
                </DropdownMenuItem>
                <DropdownMenuItem
                  onClick={() => toggleMode("alarm")}
                  className={cn("gap-2 text-xs", drawMode === "alarm" && "text-amber-400")}
                >
                  <AlarmClockPlus className="h-3 w-3" /> Alarme
                </DropdownMenuItem>
                <DropdownMenuSeparator />
                <DropdownMenuItem disabled={!hasSl} onClick={() => handleOrder(25)} className="gap-2 text-xs">25 %</DropdownMenuItem>
                <DropdownMenuItem disabled={!hasSl} onClick={() => handleOrder(50)} className="gap-2 text-xs">50 %</DropdownMenuItem>
                <DropdownMenuItem disabled={!hasSl} onClick={() => handleOrder(100)} className="gap-2 text-xs">100 %</DropdownMenuItem>
                <DropdownMenuItem
                  onClick={() => chartStore.setOrderMode(zone.zone_id, orderMode === "market" ? "limit" : "market")}
                  className="gap-2 text-xs"
                >
                  {orderMode === "market" ? "→ Mode Lmt" : "→ Mode Mkt"}
                </DropdownMenuItem>
                <DropdownMenuSeparator />
                <DropdownMenuItem
                  disabled={!hasPosition}
                  onClick={handleClose}
                  className={cn("gap-2 text-xs", hasPosition && "text-orange-400")}
                >
                  <CircleX className="h-3 w-3" /> Fermer
                </DropdownMenuItem>
                <DropdownMenuItem
                  onClick={captureStatus === "idle" ? handleCapture : undefined}
                  className="gap-2 text-xs"
                >
                  <Camera className="h-3 w-3" />
                  {captureStatus === "pending" ? "Capture…"
                   : captureStatus === "success" ? "Capturé !"
                   : "Capture"}
                </DropdownMenuItem>
                <DropdownMenuItem
                  disabled={!hasTradeId}
                  onClick={openJournal}
                  className="gap-2 text-xs"
                >
                  <NotebookPen className="h-3 w-3" /> Journal
                </DropdownMenuItem>
              </DropdownMenuContent>
            </DropdownMenu>
          )}
        </div>
      </div>

      {/* ── Common info bar — identical for every strategy ─────────────────── */}
      {/* Strategy badge · Bollinger Z · premarket vol · current-bar vol · news ·
          IA analysis (context/verdict). Strategy-specific fields are drawn on the
          chart itself (StrategyInfoOverlay), not here. Wrapped in infoBandRef so
          the screenshot capture can include it. */}
      <div ref={infoBandRef}>
        <ChartInfoBar
          zone={zone}
          card={card}
          cardInfo={cardInfo ?? null}
          enrichment={enrichment ?? null}
          dayVolume={liveState?.volume_day ?? null}
          currentBarVolume={currentBarVolume}
          onRunLlm={() => {
            if (zone.symbol && zone.strategy_id) {
              api.runAlertLlm(zone.symbol, zone.strategy_id).catch(() => {});
            }
          }}
        />

        {/* Reason + LLM summary */}
        {zone.reason && (
          <div className="line-clamp-1 px-2 text-[9px] leading-tight text-muted-foreground/60">
            {zone.reason}
          </div>
        )}
        {zone.llm_summary && (
          <div className="mx-2 mb-1 rounded bg-blue-900/20 px-1.5 py-1 text-[10px] leading-relaxed text-blue-300">
            {zone.llm_summary}
          </div>
        )}
      </div>

      {/* ── Chart area: panes from the strategy card ───────────────────────── */}
      {/* Every pane is tool-interactive: SL / TP / alarms / lines can be placed
          (and dragged) from ANY pane — they're price levels shared by the zone, so
          they render and stay in sync across panes. The `interactive` pane is only
          special in that the toolbar *timeframe* drives it (the others keep their
          card timeframe). A "daily" pane is fed from the enrichment payload
          (history + red split markers) when available, else the cached daily
          history, else the live daily aggregate. */}
      <div ref={chartAreaRef} className="relative mx-1 mb-1 mt-0.5 flex min-h-0 flex-1 overflow-hidden rounded">
        {columns.map((paneIdxs, ci) => (
          <Fragment key={ci}>
            {/* Vertical gutter between columns — drag to resize their widths. */}
            {ci > 0 && (
              <div
                onMouseDown={(e) => onGutterDown(e, "x", ci - 1, 0)}
                title="Glisser pour redimensionner les colonnes"
                className="group relative z-10 flex w-1.5 shrink-0 cursor-col-resize items-stretch"
              >
                <div className="mx-auto w-px bg-border/40 transition-colors group-hover:bg-sky-500/70" />
              </div>
            )}
            <div
              className="flex min-h-0 min-w-0 flex-col"
              style={{ flexGrow: colGrow(ci), flexBasis: 0 }}
            >
            {paneIdxs.map((i, ri) => {
              const pane          = panes[i];
              const isInteractive = i === interactiveIdx;
              const paneSymbol    = pane.symbol ?? zone.symbol!;
              const paneTf        = isInteractive ? timeframe : pane.timeframe;
              const isDaily       = paneTf === "daily";
              // Drawings belong to a timeframe class: an intraday pane only shows
              // intraday drawings, the daily pane only shows daily ones.
              const paneScope: DrawScope = isDaily ? "daily" : "intraday";
              const paneLines       = lines.filter((l) => l.scope === paneScope);
              const paneAnnotations = annotations.filter((a) => a.scope === paneScope);
              // The daily pane loads its bars through the unified path (Alpaca-fresh,
              // today's session included) like every other pane; only the split-day
              // markers come from the enrichment payload.
              const splitMarkers =
                isDaily && enrichment?.split_markers?.length
                  ? enrichment.split_markers.map((m) => ({ time: m.time, color: chartTheme.markers.split, text: m.label }))
                  : undefined;
              // HOD Drive: a HOD point (above) + LOD point (below) + a small mark
              // under every bar of the green series. Drawn on the interactive
              // (timeframe) pane only; the levels/times come from the live overlay.
              const hodMarkers =
                isHod && isInteractive && hodOverlay
                  ? [
                      ...(hodOverlay.hod_time != null
                        ? [{ time: hodOverlay.hod_time, color: "#f59e0b", position: "aboveBar" as const, shape: "circle" as const, text: "HOD" }]
                        : []),
                      ...(hodOverlay.lod_time != null
                        ? [{ time: hodOverlay.lod_time, color: "#38bdf8", position: "belowBar" as const, shape: "circle" as const, text: "LOD" }]
                        : []),
                      ...hodOverlay.series_bar_times.map((t) => ({
                        time: t, color: "#22c55e", position: "belowBar" as const, shape: "arrowUp" as const,
                      })),
                    ]
                  : undefined;
              const hasOverlay = overlayPaneIdx === i;
              return (
                <Fragment key={i}>
                  {/* Horizontal gutter between stacked panes — drag to resize heights. */}
                  {ri > 0 && (
                    <div
                      onMouseDown={(e) => onGutterDown(e, "y", ci, ri - 1)}
                      title="Glisser pour redimensionner les panneaux"
                      className="group relative z-10 flex h-1.5 shrink-0 cursor-row-resize flex-col justify-center"
                    >
                      <div className="my-auto h-px bg-border/40 transition-colors group-hover:bg-sky-500/70" />
                    </div>
                  )}
                <div
                  className={cn(
                    "relative min-h-0 min-w-0",
                    // Controller focus ring (only meaningful when there's a choice).
                    panes.length > 1 && i === focusPaneIdx && "rounded-sm ring-1 ring-sky-500/50",
                  )}
                  style={{ flexGrow: rowGrow(ci, ri), flexBasis: 0 }}
                >
                  {!isInteractive && (
                    <span className={cn(
                      "pointer-events-none absolute top-1 z-10 rounded bg-black/40 px-1 text-[8px] tabular-nums text-muted-foreground/70",
                      // Avoid the strategy overlay (top-left): label goes right when
                      // this pane carries it.
                      hasOverlay ? "right-1" : "left-1",
                    )}>
                      {paneSymbol} · {pane.timeframe}
                    </span>
                  )}
                  {/* Strategy-specific info overlay (rich for Micro Pullback). */}
                  {hasOverlay && (
                    isMicro ? (
                      <MicroInfoOverlay
                        cardInfo={cardInfo ?? null}
                        enrichment={enrichment ?? null}
                        news={tickerNews ?? []}
                      />
                    ) : isHod ? (
                      <HodDriveInfoOverlay overlay={hodOverlay ?? null} />
                    ) : (
                      <StrategyInfoOverlay
                        card={card}
                        live={liveState}
                        cardInfo={cardInfo ?? null}
                        enrichment={enrichment ?? null}
                      />
                    )
                  )}
                  <LightweightChart
                    symbol={paneSymbol}
                    timeframe={paneTf}
                    drawMode={drawMode}
                    paneScope={paneScope}
                    pendingEmoji={pendingEmoji}
                    slPrice={context?.stop_loss ?? null}
                    tpPrice={context?.take_profit ?? null}
                    entryPrice={openPosition?.avg_entry_price ?? null}
                    bid={liveState?.bid ?? null}
                    ask={liveState?.ask ?? null}
                    ordersActive={hasPosition}
                    lines={paneLines}
                    annotations={paneAnnotations}
                    alarms={alarms}
                    linePoint1={linePoint1}
                    indicators={pane.indicators}
                    markers={hodMarkers ?? splitMarkers}
                    crosshairSync={crosshairSyncRef.current}
                    paneId={`${zone.zone_id}-${i}`}
                    onPriceClick={handlePriceClick}
                    onSlDragEnd={handleSlDragEnd}
                    onTpDragEnd={handleTpDragEnd}
                    onSlDblClick={handleSlDragEnd}
                    onDeleteSl={handleDeleteSl}
                    onDeleteTp={handleDeleteTp}
                    onDeleteAlarm={handleDeleteAlarm}
                    onAlarmDragEnd={handleAlarmDragEnd}
                    onLineChange={handleLineChange}
                    onAnnotationMove={handleAnnotationMove}
                    onCreateAnnotation={(kind, time, price, text) => handleCreateAnnotation(kind, time, price, text, paneScope)}
                    onEditAnnotation={handleEditAnnotation}
                    onContextMenu={handleContextMenu}
                    onCancelTool={() => chartStore.setDrawMode(zone.zone_id, "none")}
                  />
                </div>
                </Fragment>
              );
            })}
            </div>
          </Fragment>
        ))}
      </div>

      {/* Journal modal — pinned to the trade captured at open time, so a ticker
          switch in the zone behind it can't swap its contents or unmount it. */}
      {journalTarget && (
        <JournalModal
          open={true}
          onClose={() => setJournalTarget(null)}
          tradeId={journalTarget.tradeId}
          symbol={journalTarget.symbol}
        />
      )}


      {/* Right-click context menu for a drawing / price line */}
      {ctxMenu && (() => {
        const t = ctxMenu.target;
        const lineId = t.type === "line" ? t.id : null;
        const annId  = t.type === "annotation" ? t.id : null;
        return (
          <DrawingContextMenu
            target={t}
            x={ctxMenu.x}
            y={ctxMenu.y}
            line={lineId ? lines.find((l) => l.id === lineId) : undefined}
            annotation={annId ? annotations.find((a) => a.id === annId) : undefined}
            onClose={() => setCtxMenu(null)}
            onDelete={() => {
              if (t.type === "line") handleDeleteLine(t.id);
              else if (t.type === "annotation") handleDeleteAnnotation(t.id);
              else if (t.type === "sl") handleDeleteSl();
              else if (t.type === "tp") handleDeleteTp();
              else if (t.type === "alarm") handleDeleteAlarm(t.id);
            }}
            onDuplicate={lineId ? () => handleDuplicateLine(lineId) : undefined}
            onStyleLine={lineId ? (patch) => handleStyleLine(lineId, patch) : undefined}
            onStyleAnnotation={annId ? (patch) => handleStyleAnnotation(annId, patch) : undefined}
          />
        );
      })()}
    </div>
  );
}
