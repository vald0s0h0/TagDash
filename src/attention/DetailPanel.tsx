// Detail of the selected ticker: a small 5-minute candle chart with volume over
// the visible sessions (ET clock, framed to fit them), and what the sessions
// amount to — range, current change, relative activity, flow, acceleration, float
// rotation.
//
// The candles are the very slots the map is drawn from: selecting a ticker asks
// the provider for nothing. Hiding the after-hours crops the chart and the
// figures alike, without moving the price reference.

import { useEffect, useRef, useState } from "react";
import { createChart, LineStyle, type IChartApi, type ISeriesApi, type UTCTimestamp } from "lightweight-charts";
import {
  addVolumeSeries, baseChartOptions, candleSeriesOptions, volumeColors,
} from "@/charts/chartDefaults";
import { attentionApi } from "./api";
import {
  floatRotation, fmtCount, fmtMoney, fmtPct, fmtPrice, fmtRel, PATTERNS, slotTime,
  type Shown,
} from "./model";
import type { Detail, MapView } from "./types";
import { GlyphIcon } from "./Legend";

function Stat({ label, value, sub }: { label: string; value: string; sub?: string }) {
  return (
    <div className="min-w-0">
      <div className="truncate text-[10px] uppercase tracking-wide text-muted-foreground/70">{label}</div>
      <div className="truncate text-[13px] font-semibold tabular-nums text-foreground">{value}</div>
      {sub && <div className="truncate text-[10px] tabular-nums text-muted-foreground">{sub}</div>}
    </div>
  );
}

export function DetailPanel({
  map, item, showAh, followReview, mapKey, onSend, sent,
}: {
  map: MapView;
  item: Shown;
  showAh: boolean;
  followReview: boolean;
  /** Identity of what the map shows: the candles are re-read when it moves. */
  mapKey: string;
  onSend: () => void;
  sent: boolean;
}) {
  const el = useRef<HTMLDivElement>(null);
  const chart = useRef<IChartApi | null>(null);
  const candles = useRef<ISeriesApi<"Candlestick"> | null>(null);
  const volume = useRef<ISeriesApi<"Histogram"> | null>(null);
  const [detail, setDetail] = useState<Detail | null>(null);
  const symbol = item.t.s;

  useEffect(() => {
    if (!el.current) return;
    const c = createChart(el.current, {
      ...baseChartOptions(false),
      autoSize: true,
      handleScroll: false,
      handleScale: false,
    });
    c.timeScale().applyOptions({ rightOffset: 1 });
    candles.current = c.addCandlestickSeries(candleSeriesOptions());
    volume.current = addVolumeSeries(c);
    chart.current = c;
    return () => {
      c.remove();
      chart.current = null;
    };
  }, []);

  useEffect(() => {
    let alive = true;
    attentionApi
      .getDetail(symbol, followReview)
      .then((d) => { if (alive) setDetail(d); })
      .catch(() => { if (alive) setDetail(null); });
    return () => { alive = false; };
  }, [symbol, followReview, mapKey]);

  // Reference close as a dotted line: the zero of the map's Y axis.
  const refLine = useRef<ReturnType<ISeriesApi<"Candlestick">["createPriceLine"]> | null>(null);
  useEffect(() => {
    const cs = candles.current;
    const vs = volume.current;
    if (!cs || !vs) return;
    const rows = detail && detail.s === symbol
      ? detail.candles.filter((c) => showAh || c.i >= map.frame.n_ah)
      : [];
    const vc = volumeColors();
    cs.setData(rows.map((c) => ({
      time: c.t as UTCTimestamp, open: c.o, high: c.h, low: c.l, close: c.c,
    })));
    vs.setData(rows.map((c) => ({
      time: c.t as UTCTimestamp, value: c.v, color: c.c >= c.o ? vc.up : vc.down,
    })));
    if (refLine.current) {
      cs.removePriceLine(refLine.current);
      refLine.current = null;
    }
    if (rows.length && detail) {
      refLine.current = cs.createPriceLine({
        price: detail.ref_close,
        color: "rgba(240,239,236,0.45)",
        lineWidth: 1,
        lineStyle: LineStyle.Dotted,
        axisLabelVisible: true,
        title: "clôture",
      });
    }
    // Frame exactly the sessions on screen.
    chart.current?.timeScale().fitContent();
  }, [detail, symbol, showAh, map.frame.n_ah]);

  const p = PATTERNS[item.pattern];
  const head = item.head;
  // High and low are prices touched inside a slot; their distance to the close is
  // read off the price itself (the map's extremes are slot closes, drawn there).
  const vsRef = (price: number | null) =>
    price == null ? undefined : `${fmtPct((price / item.t.ref_close - 1) * 100)} vs clôture`;
  const rotation = floatRotation(item);
  const baseTarget = map.hist_target;

  return (
    <div className="flex min-h-0 flex-col gap-2">
      <div className="flex items-start justify-between gap-2">
        <div className="min-w-0">
          <div className="flex items-baseline gap-2">
            <span className="text-lg font-bold leading-none text-foreground">{symbol}</span>
            <span className="text-[13px] tabular-nums text-foreground">{fmtPrice(item.t.last)}</span>
            <span className="text-[13px] font-semibold tabular-nums text-foreground">{fmtPct(head[2])}</span>
          </div>
          <div className="mt-0.5 truncate text-[11px] text-muted-foreground">{item.t.name ?? "—"}</div>
        </div>
        <button
          type="button"
          onClick={onSend}
          title="Ouvrir ce ticker dans la fenêtre principale"
          className="shrink-0 rounded border border-border px-2 py-1 text-[11px] text-foreground transition-colors hover:bg-accent"
        >
          {sent ? "Envoyé ✓" : "Envoyer au graphique"}
        </button>
      </div>

      <div className="flex flex-wrap items-center gap-x-2 gap-y-1 text-[11px] text-muted-foreground">
        <span className="flex items-center gap-1 text-foreground">
          <GlyphIcon pattern={item.pattern} />
          {p.label}
        </span>
        <span>· {p.hint}</span>
        {item.t.split && (
          <span className="rounded bg-accent px-1.5 py-0.5 text-foreground">
            split {item.t.split} ce matin — prix AH ramenés à la base du jour
          </span>
        )}
        {item.t.group === "unknown" && (
          <span className="rounded border border-dashed border-border px-1.5 py-0.5">capitalisation inconnue</span>
        )}
        {item.baseN < baseTarget && (
          <span className="rounded border border-dashed border-border px-1.5 py-0.5">
            base : {item.baseN}/{baseTarget} séances
          </span>
        )}
      </div>

      <div ref={el} className="h-[210px] w-full shrink-0 overflow-hidden rounded border border-border" />

      <div className="grid grid-cols-3 gap-x-3 gap-y-2">
        <Stat label="Haut" value={fmtPrice(item.hi)} sub={vsRef(item.hi)} />
        <Stat label="Bas" value={fmtPrice(item.lo)} sub={vsRef(item.lo)} />
        <Stat label="Variation" value={fmtPct(head[2])} sub={`clôture ${fmtPrice(item.t.ref_close)}`} />
        <Stat
          label="Activité relative"
          value={fmtRel(head[1])}
          sub={item.xPeak ? `pic ${fmtRel(item.xPeak.x)} · ${slotTime(map.frame, item.xPeak.i)}` : "pas de pic"}
        />
        <Stat label="Débit" value={`${fmtMoney(head[3])}/min`} sub={`${fmtMoney(item.dollars)} cumulés`} />
        <Stat
          label="Accélération"
          value={item.t.accel == null ? "n/d" : fmtRel(item.t.accel)}
          sub={item.t.accel == null ? "créneau précédent sans échange" : "vs créneau précédent"}
        />
        <div className="col-span-2">
          <Stat
            label="Rotation du flottant"
            value={rotation == null ? "indisponible" : `${rotation.toFixed(rotation >= 100 ? 0 : 1)} %`}
            sub={
              rotation == null
                ? "flottant inconnu"
                : `${fmtCount(item.vol)} échangés (${showAh ? "AH + pré-market" : "pré-market"}) / flottant ${fmtCount(item.t.float)} au ${map.float_date ?? "— date inconnue"}`
            }
          />
        </div>
        <Stat label="Capitalisation" value={item.t.cap == null ? "inconnue" : fmtMoney(item.t.cap)} />
      </div>
    </div>
  );
}
