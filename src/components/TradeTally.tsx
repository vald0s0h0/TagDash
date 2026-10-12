import { useCallback, useEffect, useRef, useState } from "react";
import { api } from "@/lib/tauri";
import { useUiStore } from "@/stores/uiStore";

/** Embedded TradeTally web app (self-hosted on the user's NAS). Full-bleed, no
 *  sidebar.
 *
 *  TradeTally refuses to be framed (`X-Frame-Options: DENY` / CSP
 *  `frame-ancestors 'none'`), so it can't live in an <iframe>. The Rust side
 *  embeds a real native child webview that loads the site as a top-level
 *  document — the frame-blocking headers don't apply, and the WebView2 data dir
 *  persists cookies + localStorage, so a one-time login (and the dark/light
 *  preference) sticks across restarts.
 *
 *  Two things to know about a native child webview: it always paints ABOVE the
 *  DOM, and it can't be reloaded/cleared from inside the page. So:
 *   - The webview lives ONLY while this tab is shown and no modal is open. We
 *     create it on enter / modal-close and destroy it on leave / modal-open —
 *     a destroyed webview can't cover a modal nor keep a WebView2 process tree
 *     resident (the cause of the silent crashes when opening a modal after
 *     visiting this tab on a memory-tight machine).
 *   - The reload / clear-cache controls live in a toolbar strip the webview
 *     never covers, because an in-page right-click menu would be hidden behind
 *     the native webview. */
export function TradeTally() {
  const hostRef = useRef<HTMLDivElement>(null);
  const openModal = useUiStore((s) => s.openModal);
  // Fallback menu on the placeholder (visible only when no webview covers it).
  const [menu, setMenu] = useState<{ x: number; y: number; armed: boolean } | null>(null);

  // Pin the native webview over the host rect (CSS px == window logical px).
  const placeWebview = useCallback(() => {
    const node = hostRef.current;
    if (!node) return;
    if (useUiStore.getState().openModal != null) return; // a modal owns the screen
    const r = node.getBoundingClientRect();
    api.tradetallySetBounds(r.left, r.top, r.width, r.height).catch((e) => {
      // Surface instead of swallowing: a failed create is exactly the "stuck on
      // Chargement…" symptom — the user can then Recharger / Vider le cache from
      // the placeholder's right-click menu.
      console.error("[tradetally] set_bounds failed:", e);
    });
  }, []);

  // Keep the webview pinned to the host rect; recreate on resize/maximize. rAF
  // lets layout settle first. Tab leave (unmount) destroys the webview.
  useEffect(() => {
    const el = hostRef.current;
    if (!el) return;

    let raf = 0;
    const sync = () => {
      cancelAnimationFrame(raf);
      raf = requestAnimationFrame(placeWebview);
    };

    sync();
    const ro = new ResizeObserver(sync);
    ro.observe(el);
    window.addEventListener("resize", sync);
    return () => {
      cancelAnimationFrame(raf);
      ro.disconnect();
      window.removeEventListener("resize", sync);
      api.tradetallyClose().catch(() => {});
    };
  }, [placeWebview]);

  // A modal would be painted behind the native webview: destroy it while one is
  // open, recreate (fresh) when it closes.
  useEffect(() => {
    if (openModal != null) {
      api.tradetallyClose().catch(() => {});
    } else {
      placeWebview();
    }
  }, [openModal, placeWebview]);

  // Fallback menu: hard refresh (destroy + recreate, which also lands on the home
  // page) — robust whether the webview is live or stuck/absent.
  const handleReload = useCallback(async () => {
    setMenu(null);
    try {
      await api.tradetallyClose();
    } catch { /* ignore */ }
    placeWebview();
  }, [placeWebview]);

  // Fallback menu: nuclear recovery for a corrupted cache. Clears cache + cookies
  // + localStorage, so it logs the user out and resets the dark/light preference
  // — hence the two-step confirm. Ensures the webview exists before clearing.
  const handleClearData = useCallback(async () => {
    setMenu(null);
    try {
      const node = hostRef.current;
      if (node) {
        const r = node.getBoundingClientRect();
        await api.tradetallySetBounds(r.left, r.top, r.width, r.height);
      }
      await api.tradetallyClearData();
    } catch (e) {
      console.error("[tradetally] clear_data failed:", e);
    }
  }, []);

  const item =
    "block w-full rounded px-2 py-1.5 text-left transition-colors hover:bg-accent";

  return (
    <div className="flex flex-1 flex-col overflow-hidden bg-background">
      {/* Webview host: the native webview is pinned over this rect. The
          placeholder shows through only until the webview paints on top (or if
          it failed to appear — then its right-click menu recovers). */}
      <div
        ref={hostRef}
        className="relative flex-1 overflow-hidden"
        onContextMenu={(e) => {
          e.preventDefault();
          setMenu({ x: e.clientX, y: e.clientY, armed: false });
        }}
        onClick={() => setMenu(null)}
      >
        <div className="flex h-full w-full flex-col items-center justify-center gap-2 text-sm text-muted-foreground">
          <span>Chargement de TradeTally…</span>
          <span className="text-xs text-muted-foreground/60">
            Clic droit : Recharger · Page d&apos;accueil · Vider cache &amp; cookies
          </span>
        </div>
        {menu && (
          <div
            className="fixed z-50 min-w-[190px] rounded-md border border-border bg-popover p-1 text-xs text-popover-foreground shadow-lg"
            style={{ left: menu.x, top: menu.y }}
            onClick={(e) => e.stopPropagation()}
          >
            <button type="button" className={item} onClick={handleReload}>Recharger</button>
            <button type="button" className={item} onClick={handleReload}>Page d&apos;accueil</button>
            <div className="mx-0.5 my-1 h-px bg-border" />
            <button
              type="button"
              className={`${item} text-red-400 hover:bg-red-900/30`}
              onClick={() => (menu.armed ? handleClearData() : setMenu({ ...menu, armed: true }))}
            >
              {menu.armed ? "Confirmer (déconnecte TradeTally)" : "Vider cache & cookies…"}
            </button>
          </div>
        )}
      </div>
    </div>
  );
}
