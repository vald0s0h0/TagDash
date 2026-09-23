import { Sunrise } from "lucide-react";
import type { PremarketMode } from "@/types";
import { cn } from "@/lib/utils";

/** Daily-pane premarket toggle. Cycles off → surimpression → premarket seul.
 *
 *  It lives ON the pane rather than in the zone toolbar because it only ever
 *  concerns a daily pane, and a zone can show a daily pane next to three intraday
 *  ones. Three states, one click each way — a dropdown for three mutually
 *  exclusive looks would cost two clicks to see the one you want.
 *
 *  Styled as a CONTROL, not as an annotation: an earlier version reused the pane
 *  label's 8px muted grey and could not be found at all. Purely presentational —
 *  its own placement (clear of the price scale) is the chart's business, since only
 *  the chart knows how wide that scale currently is. */
export function PremarketChip({ mode, onCycle }: { mode: PremarketMode; onCycle: () => void }) {
  const { label, title } = {
    off: {
      label: "PM",
      title: "Premarket masqué — afficher le 04:00–09:30 en surimpression",
    },
    overlay: {
      label: "PM ◑",
      title: "Premarket en surimpression (fantôme derrière les bougies) — n'afficher que le premarket",
    },
    only: {
      label: "PM ●",
      title: "Premarket seul, heures régulières en fond — masquer le premarket",
    },
  }[mode] ?? { label: "PM", title: "Premarket" };
  const on = mode !== "off";
  return (
    <button
      type="button"
      title={title}
      onClick={onCycle}
      className={cn(
        "flex items-center gap-0.5 rounded px-1 py-px text-[10px] font-medium leading-4 ring-1 transition-colors",
        on
          ? "bg-sky-500/20 text-sky-300 ring-sky-400/50 hover:bg-sky-500/30"
          : "bg-black/70 text-muted-foreground ring-white/15 hover:bg-black/90 hover:text-foreground",
      )}
    >
      <Sunrise className="h-2.5 w-2.5" />
      {label}
    </button>
  );
}
