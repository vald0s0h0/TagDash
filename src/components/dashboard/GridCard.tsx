import * as React from "react";
import { Layers } from "lucide-react";
import {
  DropdownMenu,
  DropdownMenuTrigger,
  DropdownMenuContent,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
} from "@/components/ui/dropdown-menu";
import { cn } from "@/lib/utils";
import {
  useDashboardStore,
  GRID_COLS,
  GRID_ROWS,
  type CardId,
  type Surface,
} from "@/stores/dashboardStore";

function clamp(v: number, lo: number, hi: number): number {
  return Math.max(lo, Math.min(hi, v));
}

const SURFACE_LABELS: Record<Surface, string> = {
  glass: "Glass",
  heavy: "Heavy",
  invisible: "Transparent",
};

/** A single card placed on the 20×12 grid. Its frosted/brutal surface (glass /
 *  heavy / transparent) is chosen per card and persisted. Cards render their own
 *  internal labels, so there is no separate header. When the dashboard is in edit
 *  mode the whole card becomes a drag handle, a bottom-right grip resizes it, and
 *  a top-right button (or right-click) picks the surface — all snap to cells. */
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
  const setSurface = useDashboardStore((s) => s.setSurface);

  const [menuOpen, setMenuOpen] = React.useState(false);

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
        `surface-${layout.surface}`,
        "relative flex min-h-0 flex-col",
        editing && "cursor-move select-none ring-1 ring-white/25"
      )}
      style={{
        gridColumn: `${layout.x + 1} / span ${layout.w}`,
        gridRow: `${layout.y + 1} / span ${layout.h}`,
      }}
      onPointerDown={startDrag}
      onContextMenu={
        editing
          ? (e) => {
              e.preventDefault();
              setMenuOpen(true);
            }
          : undefined
      }
    >
      <div className="min-h-0 flex-1 overflow-hidden">{children}</div>

      {editing && (
        <>
          {/* Surface picker — opened by the button or by right-clicking the card. */}
          <DropdownMenu open={menuOpen} onOpenChange={setMenuOpen}>
            <DropdownMenuTrigger asChild>
              <button
                title={`Design : ${SURFACE_LABELS[layout.surface]}`}
                onPointerDown={(e) => e.stopPropagation()}
                className="absolute right-1.5 top-1.5 z-[4] flex h-6 w-6 items-center justify-center rounded-md bg-black/30 text-white/70 backdrop-blur transition-colors hover:bg-black/50 hover:text-white"
              >
                <Layers className="h-3.5 w-3.5" />
              </button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end" className="w-40">
              <DropdownMenuLabel className="truncate">{title}</DropdownMenuLabel>
              <DropdownMenuSeparator />
              <DropdownMenuRadioGroup
                value={layout.surface}
                onValueChange={(v) => setSurface(id, v as Surface)}
              >
                <DropdownMenuRadioItem value="glass">Glass</DropdownMenuRadioItem>
                <DropdownMenuRadioItem value="heavy">Heavy</DropdownMenuRadioItem>
                <DropdownMenuRadioItem value="invisible">
                  Transparent
                </DropdownMenuRadioItem>
              </DropdownMenuRadioGroup>
            </DropdownMenuContent>
          </DropdownMenu>

          <div
            onPointerDown={startResize}
            title="Redimensionner"
            className="absolute bottom-0.5 right-0.5 z-[4] h-4 w-4 cursor-se-resize text-white/50 hover:text-white"
          >
            <svg viewBox="0 0 10 10" className="h-full w-full">
              <path d="M9 2 L9 9 L2 9" fill="none" stroke="currentColor" strokeWidth="1.2" />
            </svg>
          </div>
        </>
      )}
    </div>
  );
}
