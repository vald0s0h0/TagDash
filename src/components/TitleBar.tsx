import { useEffect, useMemo, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { Minus, Square, Copy, X } from "lucide-react";
import { cn } from "@/lib/utils";
import { SessionClock } from "@/components/SessionClock";
import { useStrategies, useSetStrategyEnabled } from "@/queries/useScanner";

/** Custom OS title bar (native window decorations are disabled in tauri.conf.json).
 *
 *  Layout: logo (left) · clickable strategy toggles (center) · NY session clock +
 *  window controls (right). The whole bar is draggable via `data-tauri-drag-region`;
 *  the `[&_*]:pointer-events-none [&_button]:pointer-events-auto` trick lets every
 *  decorative child fall through to the drag region while real buttons stay
 *  interactive (Tauri only starts a drag when the *clicked* element carries the
 *  attribute). Window controls + drag need the matching `core:window:*` permissions
 *  in `capabilities/default.json`. */
export function TitleBar() {
  // Resolve the window once; guard the rare non-Tauri (plain browser) dev case.
  const win = useMemo(() => {
    try {
      return getCurrentWindow();
    } catch {
      return null;
    }
  }, []);

  // Track maximized state to swap the maximize/restore glyph, kept in sync on resize.
  const [maximized, setMaximized] = useState(false);
  useEffect(() => {
    if (!win) return;
    let unlisten: (() => void) | undefined;
    win.isMaximized().then(setMaximized).catch(() => {});
    win
      .onResized(() => {
        win.isMaximized().then(setMaximized).catch(() => {});
      })
      .then((u) => {
        unlisten = u;
      })
      .catch(() => {});
    return () => unlisten?.();
  }, [win]);

  const { data: strategies = [] } = useStrategies();
  const setEnabled = useSetStrategyEnabled();

  return (
    <div
      data-tauri-drag-region
      className={cn(
        "relative z-50 flex h-9 shrink-0 select-none items-center border-b border-border bg-card",
        // Decorative children fall through to the drag region; buttons stay clickable.
        "[&_*]:pointer-events-none [&_button]:pointer-events-auto"
      )}
    >
      {/* ── Left: logo (icon only, no wordmark) ── */}
      <div className="flex items-center pl-2.5 pr-1">
        <img src="/logo.png" alt="TagDash" className="h-5 w-5" />
      </div>

      {/* ── Center: strategy toggles. Click greys out / re-enables a strategy at
          runtime (persisted backend-side; the scanner picks it up live). ── */}
      <div className="absolute left-1/2 flex -translate-x-1/2 items-center gap-1">
        {strategies.map((s) => (
          <button
            key={s.id}
            onClick={() => setEnabled.mutate({ id: s.id, enabled: !s.enabled })}
            disabled={setEnabled.isPending}
            title={
              s.enabled
                ? `${s.name} — activée (clic pour désactiver)`
                : `${s.name} — désactivée (clic pour activer)`
            }
            className={cn(
              "flex items-center gap-1.5 rounded px-2 py-1 text-[11px] font-medium transition-colors",
              s.enabled
                ? "text-foreground/90 hover:bg-accent"
                : "text-muted-foreground/40 line-through hover:bg-accent/50 hover:text-muted-foreground/70"
            )}
          >
            <span
              className={cn(
                "h-1.5 w-1.5 rounded-full",
                s.enabled ? "bg-emerald-400" : "bg-muted-foreground/30"
              )}
            />
            {s.name}
          </button>
        ))}
      </div>

      {/* ── Right: NY session clock + window controls ── */}
      <div className="ml-auto flex items-center">
        <div className="px-2">
          <SessionClock />
        </div>

        <WindowButton onClick={() => win?.minimize()} title="Réduire">
          <Minus className="h-4 w-4" />
        </WindowButton>
        <WindowButton onClick={() => win?.toggleMaximize()} title={maximized ? "Restaurer" : "Agrandir"}>
          {maximized ? <Copy className="h-3.5 w-3.5 -scale-x-100" /> : <Square className="h-3.5 w-3.5" />}
        </WindowButton>
        <WindowButton onClick={() => win?.close()} title="Fermer" danger>
          <X className="h-4 w-4" />
        </WindowButton>
      </div>
    </div>
  );
}

function WindowButton({
  children,
  onClick,
  title,
  danger,
}: {
  children: React.ReactNode;
  onClick: () => void;
  title: string;
  danger?: boolean;
}) {
  return (
    <button
      onClick={onClick}
      title={title}
      className={cn(
        "flex h-9 w-11 items-center justify-center text-muted-foreground transition-colors",
        danger
          ? "hover:bg-red-600 hover:text-white"
          : "hover:bg-accent hover:text-foreground"
      )}
    >
      {children}
    </button>
  );
}
