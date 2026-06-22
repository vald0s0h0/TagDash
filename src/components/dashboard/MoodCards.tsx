import * as React from "react";
import { useLayoutEffect, useMemo, useRef } from "react";
import { ImagePlus, Quote } from "lucide-react";
import { api } from "@/lib/tauri";
import { cn } from "@/lib/utils";
import { FrostLabel } from "./frosted";
import { useMood } from "./useDashboard";

/** Renders a phrase as upright text, with `*…*` or `**…**` spans set in italic —
 *  the design system's "italicise a word for variation" pattern, driven from the
 *  user's short.txt. Tolerant of either marker so the file's formatting always
 *  takes, and the asterisks themselves are never shown. */
function renderEmphasis(text: string): React.ReactNode {
  const re = /\*\*([^*]+)\*\*|\*([^*]+)\*/g;
  const out: React.ReactNode[] = [];
  let last = 0;
  let k = 0;
  let m: RegExpExecArray | null;
  while ((m = re.exec(text)) !== null) {
    if (m.index > last) out.push(<span key={k++}>{text.slice(last, m.index)}</span>);
    out.push(
      <em key={k++} className="italic">
        {m[1] ?? m[2]}
      </em>
    );
    last = m.index + m[0].length;
  }
  if (last < text.length) out.push(<span key={k++}>{text.slice(last)}</span>);
  return out.length ? out : text;
}

/** Empty-state nudge shown when a mood folder / file has no usable content yet.
 *  The button opens the relevant drop target so the user can fill it in place. */
function EmptyHint({
  label,
  target,
  message,
  icon,
}: {
  label: string;
  target: "images" | "short" | "long";
  message: string;
  icon: React.ReactNode;
}) {
  return (
    <div className="flex h-full w-full flex-col px-6 py-5">
      <FrostLabel>{label}</FrostLabel>
      <div className="flex flex-1 flex-col items-center justify-center gap-3 text-center text-white/45">
        {icon}
        <p className="font-body text-[13px] leading-relaxed">{message}</p>
        <button
          onClick={() => api.openMoodTarget(target).catch(() => {})}
          className="rounded-md border border-white/15 bg-white/[0.06] px-3 py-1.5 font-spacemono text-[11px] uppercase tracking-[0.10em] text-white/80 backdrop-blur transition-colors hover:bg-white/[0.12] hover:text-white"
        >
          Ouvrir le dossier
        </button>
      </div>
    </div>
  );
}

/** Card 03 · IMAGE — Inspiration. A random photo from `mood/images/`, full-bleed,
 *  with a frosted bottom band carrying a Space-Mono label and an Instrument-Serif
 *  phrase. Both re-roll on every dashboard open / refresh. */
export function InspirationCard() {
  const { data: mood } = useMood();
  const img = mood?.image;
  const phrase = mood?.short_phrase;

  if (!img && !phrase) {
    return (
      <EmptyHint
        label="Inspiration"
        target="images"
        message="Dépose des images dans le dossier Mood pour les voir ici."
        icon={<ImagePlus className="h-6 w-6" />}
      />
    );
  }

  return (
    <div
      className="relative h-full w-full"
      style={{ overflow: "clip", isolation: "isolate" }}
    >
      {img && (
        <img
          src={img.data_url}
          alt=""
          className="absolute inset-0 h-full w-full object-cover object-center"
        />
      )}

      {phrase && (
        <>
          {/* Frosted bottom band: blur masked to fade upward, over a dark gradient. */}
          <div
            className="absolute inset-x-0 bottom-0 h-[60%]"
            style={{
              backdropFilter: "blur(10px)",
              WebkitBackdropFilter: "blur(10px)",
              WebkitMaskImage: "linear-gradient(to top, #000 38%, transparent)",
              maskImage: "linear-gradient(to top, #000 38%, transparent)",
              background:
                "linear-gradient(to top, rgba(10,8,6,0.60), rgba(10,8,6,0.10) 60%, transparent)",
            }}
          />
          <div className="absolute inset-x-0 bottom-0 p-[22px]">
            <FrostLabel className="text-[10px] text-white/60">Inspiration</FrostLabel>
            <SplitHeading
              text={phrase}
              size={22}
              autoFit={false}
              lineClassName="leading-[1.08]"
              containerClassName="mt-1.5 flex w-full flex-col overflow-hidden"
            />
          </div>
        </>
      )}
    </div>
  );
}

// The forced line-break marker authored directly in short.txt.
const LINE_BREAK = "↵";

/** Splits a heading on the `↵` marker from the file. No marker → one short line;
 *  otherwise exactly two lines (any extra markers fold into the second line). */
function splitHeading(text: string): [string, string] {
  const parts = text
    .split(LINE_BREAK)
    .map((s) => s.trim())
    .filter(Boolean);
  return [parts[0] ?? "", parts.slice(1).join(" ")];
}

// How far the second line is pushed right, as a fraction of the first line's
// width. < 1 so the two lines always overlap horizontally without fully aligning.
const OFFSET_RATIO = 0.5;

/** Two-line heading: line 1 left-aligned at the edge, line 2 shifted right by half
 *  of line 1's width (staggered, partially overlapping — not right-justified). Each
 *  line stays on one line.
 *
 *  With `autoFit` (default), the font shrinks from `size` down until both lines fit
 *  the box width and height — used by the H1 card so nothing is cropped. With
 *  `autoFit={false}` the font stays at `size` and only the stagger is computed —
 *  used by the Inspiration card, whose typography size must not change. */
function SplitHeading({
  text,
  size = 46,
  autoFit = true,
  lineClassName,
  containerClassName = "flex h-full w-full flex-col justify-center overflow-hidden",
}: {
  text: string;
  size?: number;
  autoFit?: boolean;
  lineClassName?: string;
  containerClassName?: string;
}) {
  const [first, second] = useMemo(() => splitHeading(text), [text]);
  const boxRef = useRef<HTMLDivElement>(null);
  const l1Ref = useRef<HTMLDivElement>(null);
  const l2Ref = useRef<HTMLDivElement>(null);

  useLayoutEffect(() => {
    const box = boxRef.current;
    const l1 = l1Ref.current;
    if (!box || !l1) return;
    const MIN = 12;

    const fit = () => {
      const cw = box.clientWidth;
      const ch = box.clientHeight;
      if (!cw) return;
      const l2 = l2Ref.current;

      // Set a candidate size; return whether it fits and the line-2 offset it implies.
      const measure = (sz: number) => {
        l1.style.fontSize = `${sz}px`;
        if (l2) {
          l2.style.fontSize = `${sz}px`;
          l2.style.marginLeft = "0px";
        }
        const w1 = l1.scrollWidth;
        const offset = l2 ? OFFSET_RATIO * w1 : 0;
        const w2 = l2 ? l2.scrollWidth : 0;
        const widthOk = w1 <= cw && offset + w2 <= cw;
        const heightOk = !ch || l1.offsetHeight + (l2 ? l2.offsetHeight : 0) <= ch;
        return { ok: widthOk && heightOk, offset };
      };

      let bestSize = size;
      let bestOffset = 0;
      if (autoFit) {
        let lo = MIN;
        let hi = size;
        bestSize = MIN;
        while (lo <= hi) {
          const mid = (lo + hi) >> 1;
          const r = measure(mid);
          if (r.ok) {
            bestSize = mid;
            bestOffset = r.offset;
            lo = mid + 1;
          } else {
            hi = mid - 1;
          }
        }
      } else {
        // Fixed size: keep `size`, only compute the stagger offset.
        bestOffset = measure(size).offset;
      }

      l1.style.fontSize = `${bestSize}px`;
      if (l2) {
        l2.style.fontSize = `${bestSize}px`;
        l2.style.marginLeft = `${bestOffset}px`;
      }
    };

    fit();
    const ro = new ResizeObserver(fit);
    ro.observe(box);
    return () => ro.disconnect();
  }, [first, second, autoFit, size]);

  const base = "w-max max-w-full whitespace-nowrap font-display text-white";

  return (
    <div ref={boxRef} className={containerClassName}>
      <div ref={l1Ref} className={cn(base, lineClassName)} style={{ fontSize: size }}>
        {renderEmphasis(first)}
      </div>
      {second && (
        <div ref={l2Ref} className={cn(base, lineClassName)} style={{ fontSize: size }}>
          {renderEmphasis(second)}
        </div>
      )}
    </div>
  );
}

/** Card 19 · H1 — a short phrase from `mood/short.txt`, distinct from the
 *  Inspiration card, set as a two-line Instrument-Serif heading (line 1 left,
 *  line 2 right). `*…*` / `**…**` spans render italic. Re-rolls on every open. */
export function HeadingCard() {
  const { data: mood } = useMood();
  const phrase = mood?.heading_phrase;

  if (!phrase) {
    return (
      <EmptyHint
        label="Titre"
        target="short"
        message="Ajoute au moins deux phrases courtes (une par ligne) dans short.txt."
        icon={<Quote className="h-6 w-6" />}
      />
    );
  }

  return (
    <div className="h-full w-full px-[30px] py-7">
      <SplitHeading
        text={phrase}
        size={46}
        lineClassName="font-normal leading-none tracking-[-0.01em]"
      />
    </div>
  );
}

/** Card 16 · NOTE — Citation. A random long phrase from `mood/long.txt`, sized to
 *  fill the card under a Space-Mono label. Re-rolls on every open / refresh. */
export function QuoteCard() {
  const { data: mood } = useMood();
  const phrase = mood?.long_phrase;

  if (!phrase) {
    return (
      <EmptyHint
        label="Citation"
        target="long"
        message="Ajoute des citations (une par ligne) dans long.txt."
        icon={<Quote className="h-6 w-6" />}
      />
    );
  }

  return (
    <div className="flex h-full w-full flex-col justify-center overflow-hidden px-[26px] py-6">
      <FrostLabel className="mb-2.5 text-white/55">Citation</FrostLabel>
      <p className="text-pretty font-body text-[13px] leading-[1.5] text-white/[0.78]">
        {phrase}
      </p>
    </div>
  );
}
