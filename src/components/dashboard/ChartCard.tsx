import { useId } from "react";

/** Card 01 · CHART — full-bleed monochrome area chart, no axes or grid. The latest
 *  value is overlaid large (Instrument Serif) at the top-left, above a Space-Mono
 *  label. The curve and its gradient fill are pure white at low alpha, per the
 *  Frosted/Brutal design system. */
export function ChartCard({
  label,
  value,
  data,
  refLines,
  yMin,
}: {
  label: string;
  value: string;
  data: number[];
  /** Extra horizontal threshold lines (e.g. [1]), drawn very thin with no
   *  legend. The value scale expands to always keep them in view. */
  refLines?: number[];
  /** Force the Y scale's floor (e.g. 0) instead of deriving it from the data. */
  yMin?: number;
}) {
  const gid = useId();
  const W = 1000;
  const H = 300;

  let line = "";
  let area = "";
  let refYs: number[] = [];
  if (data.length >= 2) {
    const min = yMin ?? Math.min(...data, ...(refLines ?? []));
    const max = Math.max(...data, ...(refLines ?? []), yMin ?? -Infinity);
    const span = max - min || 1;
    // Leave a little vertical breathing room top and bottom.
    const top = H * 0.16;
    const bot = H * 0.96;
    const toY = (v: number) => bot - ((v - min) / span) * (bot - top);
    const pts = data.map((v, i) => {
      const x = (i / (data.length - 1)) * W;
      return [x, toY(v)] as const;
    });
    line = pts.map(([x, y], i) => `${i === 0 ? "M" : "L"}${x.toFixed(1)},${y.toFixed(1)}`).join(" ");
    area = `${line} L${W},${H} L0,${H} Z`;
    refYs = (refLines ?? []).map(toY);
  }

  return (
    <div className="relative h-full w-full overflow-hidden">
      {data.length >= 2 && (
        <svg
          className="absolute inset-0 h-full w-full"
          viewBox={`0 0 ${W} ${H}`}
          preserveAspectRatio="none"
        >
          <defs>
            <linearGradient id={gid} x1="0" y1="0" x2="0" y2="1">
              <stop offset="0%" stopColor="#fff" stopOpacity={0.22} />
              <stop offset="100%" stopColor="#fff" stopOpacity={0} />
            </linearGradient>
          </defs>
          <path d={area} fill={`url(#${gid})`} />
          {refYs.map((y, i) => (
            <line
              key={i}
              x1={0}
              y1={y}
              x2={W}
              y2={y}
              stroke="rgba(255,255,255,0.35)"
              strokeWidth={1}
              vectorEffect="non-scaling-stroke"
            />
          ))}
          <path
            d={line}
            fill="none"
            stroke="rgba(255,255,255,0.60)"
            strokeWidth={1}
            strokeLinejoin="round"
            strokeLinecap="round"
            vectorEffect="non-scaling-stroke"
          />
        </svg>
      )}

      <div className="relative px-[26px] py-6">
        <div className="font-spacemono text-[11px] uppercase tracking-[0.14em] text-white/60">
          {label}
        </div>
        <div className="mt-1 font-display text-[60px] leading-[0.86] tracking-[-0.03em] tabular-nums text-white">
          {value}
        </div>
      </div>
    </div>
  );
}
