// « Tape Agression » — thin right-hand sidebar of the replay. Two bands, buyers in
// green and sellers in red, dark when nothing trades; the tape flows DOWN at a
// constant ≈ 3.3 cm per second of wall time (the newest slice is at the top), each
// slice coloured by how aggressive that side was at that instant (see
// `aggression.ts`: dark → colour → light → white, violet when exceptional). Our own
// fills cross both bands as a thin blue line.
//
// A scrolling bitmap: every frame shifts what is drawn down by the wall time
// elapsed and paints the market time played in between at the top — so the speed
// of the flow never depends on the replay speed. A seek, a resize or a timeframe
// switch repaints the whole column from the playhead, at the current speed.

import { useEffect, useMemo, useRef, type MutableRefObject } from "react";
import type { ReviewSessionData } from "@/types";
import { aggressionColor, buildAggression, intensity } from "./aggression";
import type { PlayFill } from "./model";

/** ≈ 3.3 cm at 96 CSS px per inch. */
const PX_PER_SEC = 126;
const BG = "#080808";
const FILL_LINE = "#38bdf8";

export interface ReplayClock {
  /** Playhead (market ms). */
  ph: number;
  /** Market ms per wall ms right now (skipped blanks included). */
  speed: number;
  playing: boolean;
  /** Bumped on every jump of the playhead (seek, redraw). */
  seq: number;
}

interface Props {
  data: ReviewSessionData;
  fills: PlayFill[];
  clock: MutableRefObject<ReplayClock>;
}

export function TapeAggression({ data, fills, clock }: Props) {
  const agg = useMemo(() => buildAggression(data), [data]);
  const host = useRef<HTMLDivElement>(null);
  const cv = useRef<HTMLCanvasElement>(null);

  useEffect(() => {
    const el = host.current;
    const canvas = cv.current;
    const ctx = canvas?.getContext("2d");
    if (!el || !canvas || !ctx) return;
    const scratch = document.createElement("canvas");
    const sctx = scratch.getContext("2d")!;
    let dpr = 1;
    let W = 1;
    let H = 1;
    let bandW = 1;
    let gap = 1;
    let full = true;

    const resize = () => {
      dpr = window.devicePixelRatio || 1;
      W = Math.max(1, Math.round(el.clientWidth * dpr));
      H = Math.max(1, Math.round(el.clientHeight * dpr));
      canvas.width = scratch.width = W;
      canvas.height = scratch.height = H;
      gap = Math.max(1, Math.round(dpr));
      bandW = Math.floor((W - gap) / 2);
      full = true;
    };
    const ro = new ResizeObserver(resize);
    ro.observe(el);
    resize();

    // One device-pixel row = the market instant `t`.
    const row = (y: number, t: number) => {
      ctx.fillStyle = aggressionColor(agg, "buy", intensity(agg.buy, t));
      ctx.fillRect(0, y, bandW, 1);
      ctx.fillStyle = aggressionColor(agg, "sell", intensity(agg.sell, t));
      ctx.fillRect(bandW + gap, y, W - bandW - gap, 1);
    };
    const fillLine = (y: number) => {
      ctx.fillStyle = FILL_LINE;
      ctx.fillRect(0, Math.round(y), W, Math.max(1, Math.round(dpr)));
    };

    const repaint = (ph: number, speed: number) => {
      const msPerRow = (Math.max(0.1, speed) * 1000) / (PX_PER_SEC * dpr);
      ctx.fillStyle = BG;
      ctx.fillRect(0, 0, W, H);
      for (let y = 0; y < H; y++) row(y, ph - y * msPerRow);
      for (const f of fills) {
        const y = (ph - f.ms) / msPerRow;
        if (y >= 0 && y < H) fillLine(y);
      }
    };

    let raf = 0;
    let lastWall = performance.now();
    let lastPh = NaN;
    let lastSeq = -1;
    let acc = 0;
    const frame = (now: number) => {
      const c = clock.current;
      const wallDt = now - lastWall;
      lastWall = now;
      if (full || c.seq !== lastSeq || (!c.playing && c.ph !== lastPh)) {
        repaint(c.ph, c.speed);
        full = false;
        lastSeq = c.seq;
        lastPh = c.ph;
        acc = 0;
      } else if (c.playing && c.ph > lastPh) {
        acc += (wallDt / 1000) * PX_PER_SEC * dpr;
        const rows = Math.min(H, Math.floor(acc));
        if (rows > 0) {
          acc -= rows;
          sctx.clearRect(0, 0, W, H);
          sctx.drawImage(canvas, 0, 0);
          ctx.fillStyle = BG;
          ctx.fillRect(0, 0, W, rows);
          ctx.drawImage(scratch, 0, rows);
          const span = c.ph - lastPh;
          for (let r = 0; r < rows; r++) row(r, c.ph - (r / rows) * span);
          for (const f of fills) {
            if (f.ms > lastPh && f.ms <= c.ph) fillLine(((c.ph - f.ms) / span) * rows);
          }
          lastPh = c.ph;
        }
      }
      raf = requestAnimationFrame(frame);
    };
    raf = requestAnimationFrame(frame);
    return () => {
      cancelAnimationFrame(raf);
      ro.disconnect();
    };
  }, [agg, fills, clock]);

  return (
    <div
      className="flex w-16 shrink-0 flex-col border-l border-border"
      title={
        "Tape Agression — achats (vert) / ventes (rouge), défile vers le bas à vitesse constante.\n" +
        "Agressivité = prix traversé (mid → quote → au-delà) × taille du print.\n" +
        "Sombre → couleur → clair → blanc (top 3 %) · violet = exceptionnel · trait bleu = nos exécutions."
      }
    >
      <div className="grid shrink-0 grid-cols-2 border-b border-border text-center text-[9px] uppercase leading-5">
        <span className="text-emerald-400">Ach.</span>
        <span className="text-red-400">Vte</span>
      </div>
      <div ref={host} className="relative min-h-0 flex-1">
        <canvas ref={cv} className="absolute inset-0 h-full w-full" />
      </div>
    </div>
  );
}
