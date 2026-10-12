// Legend of the map's patterns, and the same glyphs as inline SVG for the panels —
// one drawing per pattern everywhere, so colour never carries the meaning alone.

import { PATTERN_ORDER, PATTERNS } from "./model";
import type { Pattern } from "./types";

export function GlyphIcon({ pattern, size = 12 }: { pattern: Pattern; size?: number }) {
  const { color, glyph } = PATTERNS[pattern];
  const shape = (() => {
    switch (glyph) {
      case "up":      return <path d="M6 1.5 L10.5 10 L1.5 10 Z" fill={color} />;
      case "down":    return <path d="M6 10.5 L10.5 2 L1.5 2 Z" fill={color} />;
      case "diamond": return <path d="M6 1 L11 6 L6 11 L1 6 Z" fill={color} />;
      case "steps":   return <path d="M1.5 10.5 V6.5 H6 V2 H10.5 V10.5 Z" fill={color} />;
      case "ring":
        return (
          <>
            <circle cx="6" cy="6" r="4" fill="none" stroke={color} strokeWidth="2" />
            <circle cx="6" cy="6" r="1.4" fill={color} />
          </>
        );
      default:        return <circle cx="6" cy="6" r="4" fill={color} />;
    }
  })();
  return (
    <svg width={size} height={size} viewBox="0 0 12 12" className="shrink-0" aria-hidden>
      {shape}
    </svg>
  );
}

export function Legend() {
  return (
    <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-[11px] text-muted-foreground">
      {PATTERN_ORDER.map((p) => (
        <span key={p} className="flex items-center gap-1" title={PATTERNS[p].hint}>
          <GlyphIcon pattern={p} />
          {PATTERNS[p].label}
        </span>
      ))}
      <span className="flex items-center gap-1" title="Séance after-hours précédente">
        <span className="inline-block h-[3px] w-5 rounded bg-foreground/30" />
        AH
      </span>
      <span className="flex items-center gap-1" title="Pré-market du jour">
        <span className="inline-block h-[3px] w-5 rounded bg-foreground/90" />
        pré-market
      </span>
      <span className="flex items-center gap-1" title="20:00 → 04:00 : aucune donnée, rien n'est interpolé">
        <span className="inline-block w-5 border-t border-dotted border-foreground/60" />
        nuit
      </span>
      <span className="flex items-center gap-1" title="Capitalisation inconnue, ou créneau encore en cours">
        <span className="inline-block h-2.5 w-2.5 rounded-full border border-dashed border-foreground/70" />
        cap. inconnue / en cours
      </span>
    </div>
  );
}
