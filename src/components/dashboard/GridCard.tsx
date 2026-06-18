import * as React from "react";
import { cn } from "@/lib/utils";
import {
  useDashboardStore,
  GRID_COLS,
  GRID_ROWS,
  type CardId,
} from "@/stores/dashboardStore";

function clamp(v: number, lo: number, hi: number): number {
  return Math.max(lo, Math.min(hi, v));
}

/** A single glass card placed on the 10×6 grid. When the dashboard is in edit
 *  mode, the header acts as a drag handle and a bottom-right grip resizes the
 *  card — both snap to whole grid cells. */
export function GridCard({
  id,
  title,
  children,
  gridRef,
}: {
  id: CardId;
  title: string;
  children: React.ReactNode;
  gridRef: React.RefObject<HTMLDivElement>;
}) {
  const layout = useDashboardStore((s) => s.layout[id]);
  const editing = useDashboardStore((s) => s.editing);
  const move = useDashboardStore((s) => s.move);
  const resize = useDashboardStore((s) => s.resize);

  function cellSize() {
    const rect = gridRef.current?.getBoundingClientRect();
    if (!rect) return null;
    return { cw: rect.width / GRID_COLS, ch: rect.height / GRID_ROWS };
  }

  function startDrag(e: React.PointerEvent) {
    if (!editing) return;
    const size = cellSize();
    if (!size) return;
    e.preventDefault();
    const { cw, ch } = size;
    const sx = e.clientX;
    const sy = e.clientY;
    const { x: ox, y: oy, w, h } = layout;
    const onMove = (ev: PointerEvent) => {
      const nx = clamp(ox + Math.round((ev.clientX - sx) / cw), 0, GRID_COLS - w);
      const ny = clamp(oy + Math.round((ev.clientY - sy) / ch), 0, GRID_ROWS - h);
      move(id, nx, ny);
    };
    const onUp = () => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
    };
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp);
  }

  function startResize(e: React.PointerEvent) {
    if (!editing) return;
    const size = cellSize();
    if (!size) return;
    e.preventDefault();
    e.stopPropagation();
    const { cw, ch } = size;
    const sx = e.clientX;
    const sy = e.clientY;
    const { x, y, w: ow, h: oh } = layout;
    const onMove = (ev: PointerEvent) => {
      const nw = clamp(ow + Math.round((ev.clientX - sx) / cw), 1, GRID_COLS - x);
      const nh = clamp(oh + Math.round((ev.clientY - sy) / ch), 1, GRID_ROWS - y);
      resize(id, nw, nh);
    };
    const onUp = () => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
    };
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp);
  }

  return (
    <div
      className={cn(
        "glass-card relative flex min-h-0 flex-col overflow-hidden",
        editing && "ring-1 ring-white/20"
      )}
      style={{
        gridColumn: `${layout.x + 1} / span ${layout.w}`,
        gridRow: `${layout.y + 1} / span ${layout.h}`,
      }}
    >
      <div
        onPointerDown={startDrag}
        className={cn(
          "flex shrink-0 items-center justify-between px-3 py-2 text-[11px] font-medium uppercase tracking-wider text-foreground/70",
          editing && "cursor-move select-none"
        )}
      >
        <span className="truncate">{title}</span>
      </div>

      <div className="min-h-0 flex-1 overflow-hidden px-3 pb-3">{children}</div>

      {editing && (
        <div
          onPointerDown={startResize}
          title="Redimensionner"
          className="absolute bottom-0.5 right-0.5 h-4 w-4 cursor-se-resize text-foreground/40 hover:text-foreground"
        >
          <svg viewBox="0 0 10 10" className="h-full w-full">
            <path d="M9 2 L9 9 L2 9" fill="none" stroke="currentColor" strokeWidth="1.2" />
          </svg>
        </div>
      )}
    </div>
  );
}
