// The map itself, on one canvas.
//
//   X  relative activity of a 5-minute slot (dollars / same-hour median), log.
//   Y  change since the previous regular close. Linear up to ±10 %, logarithmic
//      beyond, so a +160 % runner does not flatten everyone else on the zero line.
//
// TRAJECTORIES. One ribbon per ticker, its width the dollars per minute of each
// slot on a square-root scale COMMON to every ticker on screen. The after-hours is
// the pale tail, the premarket the full head. Two slots that are not neighbours
// are joined by a hairline (nothing traded in between — that is known, not
// missing), and the last after-hours slot is joined to the first premarket one by
// a dotted line: the night, where there is no data and none is invented.
//
// Only the best `maxTraces` tickers get a ribbon; the rest show as their head, so
// a busy morning stays fluid. Everything is still hoverable and clickable.

import { useEffect, useMemo, useRef, useState } from "react";
import {
  fmtMoney, fmtPct, fmtRel, INK, isAh, PATTERNS, slotStart, slotTime,
  type Glyph, type Shown,
} from "./model";
import type { MapMode, MapView, Pt } from "./types";

const M = { left: 52, right: 20, top: 14, bottom: 28 };
const X_MIN = 0.1;
const Y_LIN = 10; // percent: linear inside, logarithmic outside
const W_MAX = 12;
const Y_TICKS = [0, 2, 5, 10, 25, 50, 100, 200, 400, 800, 1600];

const sy = (y: number) => Math.sign(y) * Math.log1p(Math.abs(y) / Y_LIN);

function quantile(sorted: number[], q: number): number {
  if (sorted.length === 0) return 1;
  return sorted[Math.min(sorted.length - 1, Math.floor(q * sorted.length))];
}

function rgba(hex: string, a: number): string {
  const n = parseInt(hex.slice(1), 16);
  return `rgba(${(n >> 16) & 255},${(n >> 8) & 255},${n & 255},${a})`;
}

function glyphPath(ctx: CanvasRenderingContext2D, g: Glyph, x: number, y: number, r: number) {
  ctx.beginPath();
  switch (g) {
    case "up":
      ctx.moveTo(x, y - r); ctx.lineTo(x + r, y + r * 0.8); ctx.lineTo(x - r, y + r * 0.8);
      break;
    case "down":
      ctx.moveTo(x, y + r); ctx.lineTo(x + r, y - r * 0.8); ctx.lineTo(x - r, y - r * 0.8);
      break;
    case "diamond":
      ctx.moveTo(x, y - r); ctx.lineTo(x + r, y); ctx.lineTo(x, y + r); ctx.lineTo(x - r, y);
      break;
    case "steps":
      // A two-step staircase rising to the right.
      ctx.moveTo(x - r, y + r); ctx.lineTo(x - r, y + r * 0.1); ctx.lineTo(x, y + r * 0.1);
      ctx.lineTo(x, y - r); ctx.lineTo(x + r, y - r); ctx.lineTo(x + r, y + r);
      break;
    default:
      ctx.arc(x, y, r * 0.85, 0, Math.PI * 2);
  }
  ctx.closePath();
}

/** A pattern's head: its glyph, ringed in the surface colour so overlapping heads
 *  stay separate marks. « Relance » is a ring with a core. */
function drawHead(
  ctx: CanvasRenderingContext2D, g: Glyph, x: number, y: number, r: number, color: string,
) {
  ctx.lineWidth = 2;
  ctx.strokeStyle = INK.surface;
  if (g === "ring") {
    ctx.beginPath(); ctx.arc(x, y, r + 1, 0, Math.PI * 2); ctx.fillStyle = INK.surface; ctx.fill();
    ctx.beginPath(); ctx.arc(x, y, r * 0.9, 0, Math.PI * 2);
    ctx.lineWidth = 2; ctx.strokeStyle = color; ctx.stroke();
    ctx.beginPath(); ctx.arc(x, y, r * 0.3, 0, Math.PI * 2); ctx.fillStyle = color; ctx.fill();
    return;
  }
  glyphPath(ctx, g, x, y, r);
  ctx.stroke();
  ctx.fillStyle = color;
  ctx.fill();
}

interface Hit {
  x: number;
  y: number;
  item: Shown;
  pt: Pt;
  head: boolean;
}

interface Props {
  map: MapView;
  items: Shown[];
  mode: MapMode;
  maxTraces: number;
  selected: string | null;
  onPick: (symbol: string) => void;
}

export function TrajectoryMap({ map, items, mode, maxTraces, selected, onPick }: Props) {
  const wrapRef = useRef<HTMLDivElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const hitsRef = useRef<Hit[]>([]);
  const [size, setSize] = useState({ w: 0, h: 0 });
  const [hover, setHover] = useState<Hit | null>(null);

  useEffect(() => {
    const el = wrapRef.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setSize({ w: el.clientWidth, h: el.clientHeight }));
    ro.observe(el);
    setSize({ w: el.clientWidth, h: el.clientHeight });
    return () => ro.disconnect();
  }, []);

  // Scales, common to every ticker on screen.
  const scale = useMemo(() => {
    let xMax = 30;
    let yMin = -5;
    let yMax = 5;
    const rates: number[] = [];
    for (const it of items) {
      for (const p of it.pts) {
        if (p[1] > xMax) xMax = p[1];
        if (p[2] < yMin) yMin = p[2];
        if (p[2] > yMax) yMax = p[2];
        rates.push(p[3]);
      }
    }
    rates.sort((a, b) => a - b);
    return { xMax: xMax * 1.5, yMin, yMax, rateRef: Math.max(1, quantile(rates, 0.98)) };
  }, [items]);

  const hoverSym = hover?.item.t.s ?? null;

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas || size.w < 80 || size.h < 80) return;
    const dpr = window.devicePixelRatio || 1;
    canvas.width = Math.round(size.w * dpr);
    canvas.height = Math.round(size.h * dpr);
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.fillStyle = INK.surface;
    ctx.fillRect(0, 0, size.w, size.h);

    const pw = size.w - M.left - M.right;
    const ph = size.h - M.top - M.bottom;
    const lx0 = Math.log10(X_MIN);
    const lx1 = Math.log10(scale.xMax);
    const s0 = sy(scale.yMin) - 0.12;
    const s1 = sy(scale.yMax) + 0.12;
    const X = (x: number) => M.left + ((Math.log10(Math.max(x, X_MIN)) - lx0) / (lx1 - lx0)) * pw;
    const Y = (y: number) => M.top + (1 - (sy(y) - s0) / (s1 - s0)) * ph;
    const W = (rate: number) => Math.max(1, Math.min(W_MAX, Math.sqrt(rate / scale.rateRef) * W_MAX));
    const f = map.frame;

    // ── Grid and axes (recessive) ──
    ctx.font = "10px ui-monospace, monospace";
    ctx.textBaseline = "middle";
    for (let e = Math.ceil(lx0); e <= Math.floor(lx1); e++) {
      const x = X(10 ** e);
      ctx.strokeStyle = e === 0 ? INK.axis : INK.grid;
      ctx.lineWidth = 1;
      ctx.beginPath(); ctx.moveTo(x + 0.5, M.top); ctx.lineTo(x + 0.5, M.top + ph); ctx.stroke();
      ctx.fillStyle = INK.muted;
      ctx.textAlign = "center";
      ctx.fillText(e === 0 ? "×1 habituel" : `×${10 ** e >= 1000 ? `${10 ** e / 1000}k` : 10 ** e}`, x, M.top + ph + 14);
    }
    ctx.textAlign = "right";
    for (const t of Y_TICKS) {
      for (const v of t === 0 ? [0] : [t, -t]) {
        if (v < scale.yMin - 1 || v > scale.yMax + 1) continue;
        const y = Y(v);
        ctx.strokeStyle = v === 0 ? INK.axis : INK.grid;
        ctx.beginPath(); ctx.moveTo(M.left, y + 0.5); ctx.lineTo(M.left + pw, y + 0.5); ctx.stroke();
        ctx.fillStyle = INK.muted;
        ctx.fillText(v === 0 ? "clôture" : `${v > 0 ? "+" : ""}${v} %`, M.left - 6, y);
      }
    }
    ctx.textAlign = "left";
    ctx.fillStyle = INK.muted;
    ctx.fillText("activité relative du créneau 5 min →", M.left + 4, M.top + ph - 8);

    const hits: Hit[] = [];
    const focus = hoverSym ?? selected;
    // Hovering isolates a path; a standing selection only sets the others back a
    // little — the map must stay readable with a ticker open in the panel.
    const fade = hoverSym ? 0.3 : 0.62;
    const traced = new Set(items.slice(0, maxTraces).map((it) => it.t.s));
    if (focus) traced.add(focus);

    // ── Trajectories, least important first so the leaders sit on top ──
    if (mode === "traj") {
      const order = items.filter((it) => traced.has(it.t.s)).reverse();
      const focusIdx = order.findIndex((it) => it.t.s === focus);
      if (focusIdx >= 0) order.push(order.splice(focusIdx, 1)[0]);
      for (const it of order) {
        const color = PATTERNS[it.pattern].color;
        const dim = focus != null && it.t.s !== focus ? fade : 1;
        ctx.lineCap = "round";
        ctx.lineJoin = "round";
        let prev: Pt | null = null;
        for (const p of it.pts) {
          const ah = isAh(f, p[0]);
          const [px, py] = [X(p[1]), Y(p[2])];
          hits.push({ x: px, y: py, item: it, pt: p, head: false });
          if (prev) {
            const prevAh = isAh(f, prev[0]);
            const [qx, qy] = [X(prev[1]), Y(prev[2])];
            ctx.beginPath(); ctx.moveTo(qx, qy); ctx.lineTo(px, py);
            if (prevAh !== ah) {
              // 20:00 → 04:00: the night. Dotted, and only that.
              ctx.setLineDash([2, 5]);
              ctx.lineWidth = 1;
              ctx.strokeStyle = rgba(color, 0.55 * dim);
            } else if (p[0] === prev[0] + 1) {
              ctx.setLineDash([]);
              ctx.lineWidth = (W(p[3]) + W(prev[3])) / 2;
              ctx.strokeStyle = rgba(color, (ah ? 0.3 : 0.9) * dim);
            } else {
              // Slots apart with nothing traded between them.
              ctx.setLineDash([]);
              ctx.lineWidth = 1;
              ctx.strokeStyle = rgba(color, (ah ? 0.16 : 0.32) * dim);
            }
            ctx.stroke();
          }
          // A slot with no neighbour on either side still shows its own weight.
          const w = W(p[3]);
          if (w > 2) {
            ctx.beginPath(); ctx.arc(px, py, w / 2, 0, Math.PI * 2);
            ctx.fillStyle = rgba(color, (ah ? 0.3 : 0.9) * dim);
            ctx.fill();
          }
          prev = p;
        }
        ctx.setLineDash([]);

        // Hour marks along the path: where the clock was.
        const focused = it.t.s === focus;
        for (const p of it.pts) {
          const start = slotStart(f, p[0]);
          if (start % 3600 !== 0) continue;
          const [px, py] = [X(p[1]), Y(p[2])];
          ctx.beginPath(); ctx.arc(px, py, focused ? 2.2 : 1.3, 0, Math.PI * 2);
          ctx.fillStyle = focused ? INK.primary : rgba(INK.primary, 0.55 * dim);
          ctx.fill();
          if (focused) {
            ctx.fillStyle = INK.secondary;
            ctx.textAlign = "center";
            ctx.fillText(slotTime(f, p[0]).slice(0, 2) + "h", px, py - 9);
          }
        }

        // Extremes: activity peak on every ribbon, price high/low on the focused one.
        const at = (i: number) => it.pts.find((p) => p[0] === i);
        const peak = it.xPeak && at(it.xPeak.i);
        if (peak) {
          const [px, py] = [X(peak[1]), Y(peak[2])];
          ctx.beginPath(); ctx.arc(px, py, focused ? 5 : 3.2, 0, Math.PI * 2);
          ctx.lineWidth = 1.5;
          ctx.strokeStyle = rgba(INK.primary, (focused ? 0.95 : 0.6) * dim);
          ctx.stroke();
          if (focused) {
            ctx.fillStyle = INK.primary; ctx.textAlign = "left";
            ctx.fillText(`pic ${fmtRel(peak[1])} · ${slotTime(f, peak[0])}`, px + 8, py + 12);
          }
        }
        if (focused) {
          for (const [ext, label] of [[it.yMax, "haut"], [it.yMin, "bas"]] as const) {
            const p = ext && at(ext.i);
            if (!p || !ext) continue;
            const [px, py] = [X(p[1]), Y(p[2])];
            const up = label === "haut";
            ctx.beginPath();
            ctx.moveTo(px - 4, py + (up ? -5 : 5)); ctx.lineTo(px + 4, py + (up ? -5 : 5));
            ctx.lineTo(px, py + (up ? -10 : 10)); ctx.closePath();
            ctx.fillStyle = INK.primary; ctx.fill();
            ctx.textAlign = "left";
            ctx.fillText(`${label} ${fmtPct(ext.y)} · ${slotTime(f, p[0])}`, px + 8, py + (up ? -8 : 9));
          }
        }
      }
    }

    // ── Heads (and bubbles), then labels with a simple collision pass ──
    const boxes: [number, number, number, number][] = [];
    const overlaps = (b: [number, number, number, number]) =>
      boxes.some((o) => b[0] < o[2] && b[2] > o[0] && b[1] < o[3] && b[3] > o[1]);
    for (let k = items.length - 1; k >= 0; k--) {
      const it = items[k];
      const p = it.head;
      const [px, py] = [X(p[1]), Y(p[2])];
      const { color, glyph } = PATTERNS[it.pattern];
      const lead = traced.has(it.t.s);
      const dim = focus != null && it.t.s !== focus ? fade + 0.15 : 1;
      hits.push({ x: px, y: py, item: it, pt: p, head: true });
      if (mode === "bubbles") {
        const r = Math.max(3, Math.min(22, Math.sqrt(p[3] / scale.rateRef) * 20));
        ctx.beginPath(); ctx.arc(px, py, r, 0, Math.PI * 2);
        ctx.fillStyle = rgba(color, 0.55 * dim); ctx.fill();
        ctx.lineWidth = 2; ctx.strokeStyle = INK.surface; ctx.stroke();
        drawHead(ctx, glyph, px, py, 3.5, rgba(color, dim));
      } else if (lead) {
        drawHead(ctx, glyph, px, py, it.t.s === focus ? 7 : 5.5, rgba(color, dim));
      } else {
        ctx.beginPath(); ctx.arc(px, py, 2.4, 0, Math.PI * 2);
        ctx.fillStyle = rgba(color, 0.6 * dim); ctx.fill();
      }
      // Still filling, or a market cap nobody collected: said with a ring, not a hue.
      if ((p[4] & 2) !== 0 || it.t.group === "unknown") {
        ctx.setLineDash([2, 2]);
        ctx.beginPath(); ctx.arc(px, py, (lead ? 9 : 5.5), 0, Math.PI * 2);
        ctx.lineWidth = 1; ctx.strokeStyle = rgba(INK.secondary, 0.8 * dim); ctx.stroke();
        ctx.setLineDash([]);
      }
    }
    ctx.textBaseline = "middle";
    ctx.textAlign = "left";
    const labelled = [...items].sort(
      (a, b) => Number(b.t.s === focus) - Number(a.t.s === focus) || b.score - a.score,
    );
    for (const it of labelled) {
      const isFocus = it.t.s === focus;
      if (!isFocus && !traced.has(it.t.s) && mode === "traj") continue;
      const [px, py] = [X(it.head[1]), Y(it.head[2])];
      const text = it.t.s + (it.t.group === "unknown" ? " ?" : "");
      ctx.font = `${isFocus ? "700 12px" : "600 11px"} ui-sans-serif, system-ui, sans-serif`;
      const w = ctx.measureText(text).width;
      const box: [number, number, number, number] = [px + 9, py - 7, px + 11 + w, py + 7];
      if (!isFocus && overlaps(box)) continue;
      boxes.push(box);
      ctx.lineWidth = 3; ctx.strokeStyle = INK.surface;
      ctx.strokeText(text, px + 10, py);
      ctx.fillStyle = focus != null && !isFocus ? rgba(INK.primary, fade + 0.1) : INK.primary;
      ctx.fillText(text, px + 10, py);
    }
    hitsRef.current = hits;
  }, [size, items, map.frame, scale, mode, maxTraces, selected, hoverSym]);

  const nearest = (e: React.MouseEvent): Hit | null => {
    const rect = canvasRef.current?.getBoundingClientRect();
    if (!rect) return null;
    const [mx, my] = [e.clientX - rect.left, e.clientY - rect.top];
    let best: Hit | null = null;
    let bestD = 14 * 14;
    for (const h of hitsRef.current) {
      // A head wins over a point of someone else's path at equal distance.
      const d = (h.x - mx) ** 2 + (h.y - my) ** 2 - (h.head ? 30 : 0);
      if (d < bestD) { bestD = d; best = h; }
    }
    return best;
  };

  const tip = hover && (() => {
    const { item: it, pt } = hover;
    const p = PATTERNS[it.pattern];
    const left = Math.min(hover.x + 16, size.w - 236);
    const top = Math.min(Math.max(hover.y - 20, 4), size.h - 132);
    return (
      <div
        className="pointer-events-none absolute z-10 w-[220px] rounded-md border border-border bg-zinc-900/95 px-2.5 py-2 text-[11px] shadow-xl"
        style={{ left, top }}
      >
        <div className="flex items-baseline justify-between gap-2">
          <span className="text-sm font-bold text-foreground">{it.t.s}</span>
          <span className="truncate text-[10px] text-muted-foreground">{it.t.name ?? ""}</span>
        </div>
        <div className="mt-0.5 flex items-center gap-1.5 text-muted-foreground">
          <span className="inline-block h-2 w-2 rounded-full" style={{ background: p.color }} />
          {p.label}
        </div>
        <div className="mt-1.5 grid grid-cols-2 gap-x-2 gap-y-0.5 tabular-nums">
          <span className="text-muted-foreground">
            {slotTime(map.frame, pt[0])} ET · {isAh(map.frame, pt[0]) ? "AH" : "PM"}
          </span>
          <span className="text-right text-foreground">{fmtPct(pt[2])}</span>
          <span className="text-muted-foreground">Activité relative</span>
          <span className="text-right text-foreground">{fmtRel(pt[1])}</span>
          <span className="text-muted-foreground">Débit</span>
          <span className="text-right text-foreground">{fmtMoney(pt[3])}/min</span>
        </div>
        {(pt[4] & 2) !== 0 && (
          <div className="mt-1 text-[10px] text-muted-foreground">Créneau en cours (5 min glissantes)</div>
        )}
        <div className="mt-1.5 border-t border-border pt-1 text-[10px] text-muted-foreground/80">
          {selected === it.t.s ? "Clic : envoyer au graphique principal" : "Clic : ouvrir le détail"}
        </div>
      </div>
    );
  })();

  return (
    <div ref={wrapRef} className="relative h-full w-full overflow-hidden">
      <canvas
        ref={canvasRef}
        style={{ width: size.w, height: size.h, cursor: hover ? "pointer" : "default" }}
        onMouseMove={(e) => {
          const h = nearest(e);
          setHover((cur) => (cur?.item.t.s === h?.item.t.s && cur?.pt === h?.pt ? cur : h));
        }}
        onMouseLeave={() => setHover(null)}
        onClick={(e) => {
          const h = nearest(e);
          if (h) onPick(h.item.t.s);
        }}
      />
      {tip}
      <div className="pointer-events-none absolute bottom-8 right-5 text-[10px] text-muted-foreground/70">
        {mode === "traj" ? "épaisseur" : "taille"} = $/min sur 5 min (√) · plein ≈ {fmtMoney(scale.rateRef)}/min
      </div>
    </div>
  );
}
