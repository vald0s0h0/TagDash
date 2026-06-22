import { useLayoutEffect, useRef } from "react";
import { cn } from "@/lib/utils";

/** Renders `text` at the largest whole-pixel font-size that still fits inside its
 *  box, re-fitting whenever the text or the box changes size (card resize, window
 *  resize). Wrapping is allowed, so the binding constraint is height; a single
 *  over-wide word is the only thing the width check guards against.
 *
 *  The font-size is mutated imperatively (no React state), so a re-fit never
 *  triggers a re-render — which keeps the ResizeObserver loop from feeding back. */
export function AutoFitText({
  text,
  className,
  min = 11,
  max = 140,
}: {
  text: string;
  className?: string;
  min?: number;
  max?: number;
}) {
  const boxRef = useRef<HTMLDivElement>(null);
  const textRef = useRef<HTMLDivElement>(null);

  useLayoutEffect(() => {
    const box = boxRef.current;
    const el = textRef.current;
    if (!box || !el) return;

    const fit = () => {
      const cw = box.clientWidth;
      const ch = box.clientHeight;
      if (cw === 0 || ch === 0) return;

      // Binary-search the largest size where the text overflows neither axis.
      let lo = min;
      let hi = max;
      let best = min;
      while (lo <= hi) {
        const mid = (lo + hi) >> 1;
        el.style.fontSize = `${mid}px`;
        if (el.scrollWidth <= cw && el.scrollHeight <= ch) {
          best = mid;
          lo = mid + 1;
        } else {
          hi = mid - 1;
        }
      }
      el.style.fontSize = `${best}px`;
    };

    fit();
    const ro = new ResizeObserver(fit);
    ro.observe(box);
    return () => ro.disconnect();
  }, [text, min, max]);

  return (
    <div ref={boxRef} className="flex h-full w-full items-center justify-center overflow-hidden">
      <div
        ref={textRef}
        className={cn("w-full text-center leading-tight", className)}
        style={{ fontSize: max }}
      >
        {text}
      </div>
    </div>
  );
}
