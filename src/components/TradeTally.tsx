import { useEffect, useRef } from "react";
import { api } from "@/lib/tauri";
import { useUiStore } from "@/stores/uiStore";

/** Embedded TradeTally web app (self-hosted on the user's NAS). Full-bleed, no
 *  sidebar.
 *
 *  TradeTally refuses to be framed (`X-Frame-Options: DENY` / CSP
 *  `frame-ancestors 'none'`), so it can't live in an <iframe>. Instead the Rust
 *  side embeds a real native child webview that loads the site as a top-level
 *  document — the frame-blocking headers don't apply, and the webview persists
 *  cookies + localStorage in the app's user-data dir, so a one-time login sticks
 *  across restarts.
 *
 *  This component just drives that webview's geometry: it keeps the native
 *  webview pinned over this container's rect and hides it when the tab is left or
 *  a modal opens (native webviews always paint above the DOM). */
export function TradeTally() {
  const containerRef = useRef<HTMLDivElement>(null);
  const openModal = useUiStore((s) => s.openModal);

  // Pin the native webview to this container's rect (CSS px == window logical px).
  // ResizeObserver covers window resize / maximize; rAF lets layout settle first.
  useEffect(() => {
    const el = containerRef.current;
    if (!el) return;

    let raf = 0;
    const sync = () => {
      cancelAnimationFrame(raf);
      raf = requestAnimationFrame(() => {
        const node = containerRef.current;
        if (!node) return;
        // A modal is rendered by the main DOM webview and would be hidden behind
        // the native webview — step aside while one is open.
        if (useUiStore.getState().openModal != null) {
          api.tradetallyHide().catch(() => {});
          return;
        }
        const r = node.getBoundingClientRect();
        api.tradetallySetBounds(r.left, r.top, r.width, r.height).catch(() => {});
      });
    };

    sync();
    const ro = new ResizeObserver(sync);
    ro.observe(el);
    window.addEventListener("resize", sync);
    return () => {
      cancelAnimationFrame(raf);
      ro.disconnect();
      window.removeEventListener("resize", sync);
      // Leaving the tab: hide so the webview doesn't bleed over other views.
      api.tradetallyHide().catch(() => {});
    };
  }, []);

  // Toggle visibility as modals open/close while staying on this tab.
  useEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    if (openModal != null) {
      api.tradetallyHide().catch(() => {});
    } else {
      const r = el.getBoundingClientRect();
      api.tradetallySetBounds(r.left, r.top, r.width, r.height).catch(() => {});
    }
  }, [openModal]);

  return (
    <div ref={containerRef} className="relative flex-1 overflow-hidden bg-background">
      {/* Visible only until the native webview paints on top (or if it fails). */}
      <div className="flex h-full w-full items-center justify-center text-sm text-muted-foreground">
        Chargement de TradeTally…
      </div>
    </div>
  );
}
