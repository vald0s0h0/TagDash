import { useState, useRef, useEffect, useCallback } from "react";
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
  Type,
} from "lucide-react";
import { JournalModal } from "./JournalModal";
import { useQuery } from "@tanstack/react-query";
import type { AlertSignal, PaneSpec, Timeframe, ZoneAssignment } from "@/types";
import { useLayoutStore } from "@/stores/layoutStore";
import { useChartStore, type DrawMode } from "@/stores/chartStore";
import { useStrategyCards } from "@/queries/useScanner";
import { api } from "@/lib/tauri";
import { createCrosshairSync } from "@/lib/crosshairSync";
import { nyFilenameStamp } from "@/lib/nyTime";
import { LightweightChart } from "./LightweightChart";
import { EnrichmentBand } from "./EnrichmentBand";
import { cn } from "@/lib/utils";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import {
  PRIORITY_STYLES, TIMEFRAMES, resolveFieldValue,
  FieldChip, TBtn, Sep, TextInput, EmptyZone,
} from "./chartZoneParts";

// ─── Main ChartZone ───────────────────────────────────────────────────────────

interface ChartZoneProps {
  zone: ZoneAssignment;
}

export function ChartZone({ zone }: ChartZoneProps) {
  const [isDragOver, setIsDragOver] = useState(false);
  const headerRef   = useRef<HTMLDivElement>(null);
  const chartAreaRef = useRef<HTMLDivElement>(null);
  const infoBandRef  = useRef<HTMLDivElement>(null);
  // One crosshair-sync group per zone: hovering a pane mirrors the crosshair on
  // the zone's other same-instrument panes.
  const crosshairSyncRef = useRef(createCrosshairSync());
  const [narrowToolbar, setNarrowToolbar] = useState(false);

  // Text annotation pending state
  const [pendingText, setPendingText] = useState<{
    time: number; price: number; pixelX: number; pixelY: number;
  } | null>(null);

  // Journal modal
  const [journalOpen, setJournalOpen] = useState(false);

  // Capture status: idle | pending | success | error
  const [captureStatus, setCaptureStatus] = useState<"idle" | "pending" | "success" | "error">("idle");

  const placeAlertInZone = useLayoutStore((s) => s.placeAlertInZone);
  const releaseZone      = useLayoutStore((s) => s.releaseZone);

  const chartStore   = useChartStore();
  const zoneState    = useChartStore((s) => s.getZone(zone.zone_id));
  const { timeframe, drawMode, orderMode, lines, annotations, alarms, linePoint1, context } = zoneState;

  const hasSl      = context?.stop_loss  != null;
  const hasTp      = context?.take_profit != null;
  const hasTradeId = !!context?.trade_id;

  // ── Strategy identity card → panes + info-band fields ──────────────────────
  const { data: cards } = useStrategyCards();
  const card  = zone.strategy_id ? cards?.[zone.strategy_id] ?? null : null;
  // Panes to render (fallback = a single interactive pane at the toolbar tf).
  const panes: PaneSpec[] = card?.panes?.length
    ? card.panes
    : [{ timeframe, symbol: null, indicators: [], interactive: true }];
  // The pane that carries SL/TP/orders/drawing (the 5s pane for micro_pullback).
  const interactiveIdx = Math.max(0, panes.findIndex((p) => p.interactive));

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

  // ── Per-symbol info-band extras (score / market cap / float) ───────────────
  // Not in the live snapshot — fetched from the DB (mean_reversion_scores +
  // universe_assets). Static within a day, so a long stale time is fine.
  const { data: cardInfo } = useQuery({
    queryKey: ["card_info", zone.symbol],
    queryFn:  () => api.getCardInfo(zone.symbol!),
    enabled:  !!zone.symbol,
    staleTime: 5 * 60 * 1000,
  });

  // ── Load existing context when zone symbol changes ─────────────────────────
  useEffect(() => {
    if (!zone.symbol) return;
    api.getZoneTradeContext(zone.zone_id).then((ctx) => {
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
      const lines = rows
        .filter((d) => d.kind === "line" && d.t2 != null && d.p2 != null)
        .map((d) => ({
          id: d.id,
          point1: { time: d.t1, price: d.p1 },
          point2: { time: d.t2!, price: d.p2! },
        }));
      const anns = rows
        .filter((d) => d.kind === "text")
        .map((d) => ({ id: d.id, time: d.t1, price: d.p1, text: d.text ?? "", pixelX: 0, pixelY: 0 }));
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
      api.getZoneTradeContext(zone.zone_id).then((ctx) => {
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
    pixelX: number,
    pixelY: number,
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
        const line = { id: `line-${Date.now()}`, point1: lp1, point2: { time, price } };
        chartStore.addLine(zone.zone_id, line);
        chartStore.setLinePoint1(zone.zone_id, null);
        chartStore.setDrawMode(zone.zone_id, "none");
        // Memorise on the ticker (persisted, shown on every chart of this symbol).
        if (sym) {
          api.createDrawing({
            id: line.id, symbol: sym, kind: "line",
            t1: lp1.time, p1: lp1.price, t2: time, p2: price, text: null,
          }).catch(() => {});
        }
      }
    } else if (mode === "text") {
      setPendingText({ time, price, pixelX, pixelY });
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

  // ── Text annotation confirm ────────────────────────────────────────────────
  const handleTextConfirm = useCallback((text: string) => {
    if (!pendingText) return;
    const ann = {
      id:    `ann-${Date.now()}`,
      time:  pendingText.time,
      price: pendingText.price,
      text,
      pixelX: pendingText.pixelX,
      pixelY: pendingText.pixelY,
    };
    chartStore.addAnnotation(zone.zone_id, ann);
    chartStore.setDrawMode(zone.zone_id, "none");
    setPendingText(null);
    // Memorise on the ticker (persisted, shown on every chart of this symbol).
    if (zone.symbol) {
      api.createDrawing({
        id: ann.id, symbol: zone.symbol, kind: "text",
        t1: ann.time, p1: ann.price, t2: null, p2: null, text,
      }).catch(() => {});
    }
  }, [pendingText, zone.zone_id, zone.symbol, chartStore]);

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

  // ── Drag-and-drop handlers (alert → zone only; zone-to-zone removed) ────────
  function handleDragOver(e: React.DragEvent) {
    e.preventDefault();
    e.dataTransfer.dropEffect = "copy";
    setIsDragOver(true);
  }
  function handleDragLeave(e: React.DragEvent) {
    if (!e.currentTarget.contains(e.relatedTarget as Node)) setIsDragOver(false);
  }
  function handleDrop(e: React.DragEvent) {
    e.preventDefault();
    setIsDragOver(false);
    const alertData = e.dataTransfer.getData("application/tagdash-alert");
    if (alertData) {
      try { placeAlertInZone(JSON.parse(alertData) as AlertSignal, zone.zone_id); } catch { /* */ }
    }
  }

  // ── Empty zone ─────────────────────────────────────────────────────────────
  if (!zone.symbol) {
    return <EmptyZone zone={zone} onDrop={handleDrop} />;
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
      onClick={() => hasTradeId && setJournalOpen(true)}
    >
      <NotebookPen className="h-3 w-3" />
    </TBtn>
  );

  // Primary = always visible; secondary = in overflow when narrow
  const primaryButtons  = [tbRelease, tbSl, tbTp, tbAlarm, tbClock];
  const secondaryButtons = [tbLine, tbText, tbSize25, tbSize50, tbSize100, tbOrderMode, tbClose, tbCapture, tbJournal];

  // ── Render ─────────────────────────────────────────────────────────────────

  return (
    <div
      className={cn(
        "relative flex h-full w-full flex-col overflow-hidden rounded-md border border-border bg-card transition-colors",
        styles?.accent,
        isDragOver && "border-blue-500/70 bg-blue-900/5"
      )}
      onDragOver={handleDragOver}
      onDragLeave={handleDragLeave}
      onDrop={handleDrop}
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
                  onClick={() => hasTradeId && setJournalOpen(true)}
                  className="gap-2 text-xs"
                >
                  <NotebookPen className="h-3 w-3" /> Journal
                </DropdownMenuItem>
              </DropdownMenuContent>
            </DropdownMenu>
          )}
        </div>
      </div>

      {/* ── Info band: strategy name (common) + strategy-specific fields ───── */}
      {/* Common info (name + priority badge) lives in the header above. When the
          strategy declares enrichment, an async-filled EnrichmentBand is shown;
          otherwise a generic chip row resolved from the live snapshot.
          Wrapped in infoBandRef so the screenshot capture can include it. */}
      <div ref={infoBandRef}>
        <div className="flex flex-wrap items-center gap-x-2.5 gap-y-0.5 px-2 pt-1">
          {zone.strategy_name && (
            <span className="shrink-0 text-[10px] text-muted-foreground truncate">
              {zone.strategy_name}
            </span>
          )}
          {!enrichment &&
            card?.info_fields.map((f) => (
              <FieldChip key={f.key} field={f} value={resolveFieldValue(f.key, liveState, cardInfo ?? null)} />
            ))}
          {zone.price != null && (
            <span className="text-xs font-medium tabular-nums ml-auto shrink-0">
              ${zone.price.toFixed(2)}
            </span>
          )}
        </div>
        {enrichment && (
          <EnrichmentBand
            e={enrichment}
            onRunLlm={() => {
              if (zone.symbol && zone.strategy_id) {
                api.runAlertLlm(zone.symbol, zone.strategy_id).catch(() => {});
              }
            }}
          />
        )}

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
      <div ref={chartAreaRef} className="relative mx-1 mb-1 mt-0.5 flex min-h-0 flex-1 gap-1 overflow-hidden rounded">
        {panes.map((pane, i) => {
          const isInteractive = i === interactiveIdx;
          const paneSymbol    = pane.symbol ?? zone.symbol!;
          const isDaily       = pane.timeframe === "daily";
          // The daily pane loads its bars through the unified path (Alpaca-fresh,
          // today's session included) like every other pane; only the split-day
          // markers come from the enrichment payload.
          const splitMarkers =
            isDaily && enrichment?.split_markers?.length
              ? enrichment.split_markers.map((m) => ({ time: m.time, color: "#ef4444", text: m.label }))
              : undefined;
          return (
            <div
              key={i}
              className={cn("relative min-w-0 flex-1", i > 0 && "border-l border-border/40")}
            >
              {!isInteractive && (
                <span className="pointer-events-none absolute left-1 top-1 z-10 rounded bg-black/40 px-1 text-[8px] tabular-nums text-muted-foreground/70">
                  {paneSymbol} · {pane.timeframe}
                </span>
              )}
              <LightweightChart
                symbol={paneSymbol}
                timeframe={isInteractive ? timeframe : pane.timeframe}
                drawMode={drawMode}
                slPrice={context?.stop_loss ?? null}
                tpPrice={context?.take_profit ?? null}
                entryPrice={openPosition?.avg_entry_price ?? null}
                bid={liveState?.bid ?? null}
                ask={liveState?.ask ?? null}
                ordersActive={hasPosition}
                lines={lines}
                annotations={annotations}
                alarms={alarms}
                linePoint1={linePoint1}
                indicators={pane.indicators}
                markers={splitMarkers}
                crosshairSync={crosshairSyncRef.current}
                paneId={`${zone.zone_id}-${i}`}
                onPriceClick={handlePriceClick}
                onSlDragEnd={handleSlDragEnd}
                onTpDragEnd={handleTpDragEnd}
                onSlDblClick={handleSlDragEnd}
                onDeleteSl={handleDeleteSl}
                onDeleteTp={handleDeleteTp}
                onDeleteAlarm={handleDeleteAlarm}
              />

              {/* Text annotation input overlay — lives in the interactive pane */}
              {isInteractive && pendingText && (
                <div
                  className="absolute z-10"
                  style={{ left: pendingText.pixelX, top: pendingText.pixelY, transform: "translate(-50%, -110%)" }}
                >
                  <TextInput
                    onConfirm={handleTextConfirm}
                    onCancel={() => { setPendingText(null); chartStore.setDrawMode(zone.zone_id, "none"); }}
                  />
                </div>
              )}
            </div>
          );
        })}
      </div>

      {/* Journal modal */}
      {journalOpen && hasTradeId && (
        <JournalModal
          open={journalOpen}
          onClose={() => setJournalOpen(false)}
          tradeId={context!.trade_id!}
          symbol={zone.symbol!}
        />
      )}
    </div>
  );
}
