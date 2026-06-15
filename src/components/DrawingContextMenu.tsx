import { useEffect, useRef, useState } from "react";
import { Settings2, Copy, Trash2, Pencil } from "lucide-react";
import type { ChartLine, ChartAnnotation, CtxTarget, LineStyleName } from "@/stores/chartStore";
import { DRAW_COLORS } from "@/stores/drawingPrefsStore";
import { cn } from "@/lib/utils";

interface Props {
  target: CtxTarget;
  x: number;
  y: number;
  line?: ChartLine;
  annotation?: ChartAnnotation;
  onClose: () => void;
  onDelete: () => void;
  onDuplicate?: () => void;
  onEditText?: () => void;
  onStyleLine?: (patch: Partial<ChartLine>) => void;
  onStyleAnnotation?: (patch: Partial<ChartAnnotation>) => void;
}

const LINE_STYLES: { value: LineStyleName; label: string }[] = [
  { value: "solid",  label: "─────" },
  { value: "dashed", label: "— — —" },
  { value: "dotted", label: "· · · ·" },
];

/** Right-click context menu for a chart drawing or price line. Positioned at the
 *  cursor (fixed), clamped to the viewport; opens an inline style panel for lines
 *  and text/emoji. Closes on outside-click or Escape. */
export function DrawingContextMenu({
  target, x, y, line, annotation,
  onClose, onDelete, onDuplicate, onEditText, onStyleLine, onStyleAnnotation,
}: Props) {
  const ref = useRef<HTMLDivElement>(null);
  const [showStyle, setShowStyle] = useState(false);

  useEffect(() => {
    const onDown = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) onClose();
    };
    const onKey = (e: KeyboardEvent) => { if (e.key === "Escape") onClose(); };
    // Defer so the opening right-click doesn't immediately close it.
    const t = setTimeout(() => {
      window.addEventListener("mousedown", onDown);
      window.addEventListener("keydown", onKey);
    }, 0);
    return () => {
      clearTimeout(t);
      window.removeEventListener("mousedown", onDown);
      window.removeEventListener("keydown", onKey);
    };
  }, [onClose]);

  const isLine = target.type === "line";
  const isAnn  = target.type === "annotation";
  const isPriceLine = target.type === "sl" || target.type === "tp" || target.type === "alarm";
  const canStyle = (isLine && !!line) || (isAnn && !!annotation);

  // Clamp to viewport.
  const left = Math.min(x, window.innerWidth - 220);
  const top  = Math.min(y, window.innerHeight - 280);

  return (
    <div
      ref={ref}
      style={{ left, top }}
      className="fixed z-50 min-w-[10rem] rounded-md border border-border bg-zinc-900 py-1 text-xs shadow-xl"
    >
      {canStyle && (
        <button
          onClick={() => setShowStyle((s) => !s)}
          className="flex w-full items-center gap-2 px-2.5 py-1.5 text-left text-foreground/90 hover:bg-accent"
        >
          <Settings2 className="h-3.5 w-3.5" /> Paramètres…
        </button>
      )}
      {isAnn && annotation?.kind === "text" && onEditText && (
        <button
          onClick={() => { onEditText(); onClose(); }}
          className="flex w-full items-center gap-2 px-2.5 py-1.5 text-left text-foreground/90 hover:bg-accent"
        >
          <Pencil className="h-3.5 w-3.5" /> Éditer
        </button>
      )}
      {isLine && onDuplicate && (
        <button
          onClick={() => { onDuplicate(); onClose(); }}
          className="flex w-full items-center gap-2 px-2.5 py-1.5 text-left text-foreground/90 hover:bg-accent"
        >
          <Copy className="h-3.5 w-3.5" /> Dupliquer
        </button>
      )}
      <button
        onClick={() => { onDelete(); onClose(); }}
        className="flex w-full items-center gap-2 px-2.5 py-1.5 text-left text-rose-400 hover:bg-rose-900/30"
      >
        <Trash2 className="h-3.5 w-3.5" /> Supprimer
      </button>

      {showStyle && isLine && line && onStyleLine && (
        <div className="mt-1 border-t border-border px-2.5 py-2">
          <Swatches color={line.color} onPick={(color) => onStyleLine({ color })} />
          <OpacitySlider value={line.opacity} onChange={(opacity) => onStyleLine({ opacity })} />
          <WidthPicker value={line.width} onChange={(width) => onStyleLine({ width })} />
          <StylePicker value={line.lineStyle} onChange={(lineStyle) => onStyleLine({ lineStyle })} />
        </div>
      )}
      {showStyle && isAnn && annotation && onStyleAnnotation && (
        <div className="mt-1 border-t border-border px-2.5 py-2">
          <Swatches color={annotation.color} onPick={(color) => onStyleAnnotation({ color })} />
          <OpacitySlider value={annotation.opacity} onChange={(opacity) => onStyleAnnotation({ opacity })} />
          <FontSizePicker value={annotation.fontSize} onChange={(fontSize) => onStyleAnnotation({ fontSize })} />
        </div>
      )}

      {isPriceLine && !canStyle && null}
    </div>
  );
}

// ─── Style controls ─────────────────────────────────────────────────────────

function Row({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="mb-2 last:mb-0">
      <div className="mb-1 text-[9px] uppercase tracking-wide text-muted-foreground/50">{label}</div>
      {children}
    </div>
  );
}

function Swatches({ color, onPick }: { color: string; onPick: (c: string) => void }) {
  return (
    <Row label="Couleur">
      <div className="flex flex-wrap gap-1">
        {DRAW_COLORS.map((c) => (
          <button
            key={c}
            onClick={() => onPick(c)}
            style={{ background: c }}
            className={cn("h-4 w-4 rounded-sm border", color.toLowerCase() === c.toLowerCase() ? "border-white" : "border-transparent")}
          />
        ))}
      </div>
    </Row>
  );
}

function OpacitySlider({ value, onChange }: { value: number; onChange: (v: number) => void }) {
  return (
    <Row label={`Opacité ${Math.round(value * 100)}%`}>
      <input
        type="range" min={0.1} max={1} step={0.05} value={value}
        onChange={(e) => onChange(parseFloat(e.target.value))}
        className="w-full accent-amber-500"
      />
    </Row>
  );
}

function WidthPicker({ value, onChange }: { value: number; onChange: (v: number) => void }) {
  return (
    <Row label="Épaisseur">
      <div className="flex gap-1">
        {[1, 2, 3, 4, 6].map((w) => (
          <button
            key={w}
            onClick={() => onChange(w)}
            className={cn("flex-1 rounded-sm border py-1 text-[10px]", value === w ? "border-amber-500 text-foreground" : "border-border text-muted-foreground hover:text-foreground")}
          >
            {w}
          </button>
        ))}
      </div>
    </Row>
  );
}

function StylePicker({ value, onChange }: { value: LineStyleName; onChange: (v: LineStyleName) => void }) {
  return (
    <Row label="Style">
      <div className="flex gap-1">
        {LINE_STYLES.map((s) => (
          <button
            key={s.value}
            onClick={() => onChange(s.value)}
            className={cn("flex-1 rounded-sm border py-1 text-[10px] tracking-tighter", value === s.value ? "border-amber-500 text-foreground" : "border-border text-muted-foreground hover:text-foreground")}
          >
            {s.label}
          </button>
        ))}
      </div>
    </Row>
  );
}

function FontSizePicker({ value, onChange }: { value: number; onChange: (v: number) => void }) {
  return (
    <Row label="Taille">
      <div className="flex gap-1">
        {[12, 16, 20, 24, 32].map((s) => (
          <button
            key={s}
            onClick={() => onChange(s)}
            className={cn("flex-1 rounded-sm border py-1 text-[10px]", value === s ? "border-amber-500 text-foreground" : "border-border text-muted-foreground hover:text-foreground")}
          >
            {s}
          </button>
        ))}
      </div>
    </Row>
  );
}
