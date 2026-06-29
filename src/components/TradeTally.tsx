import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { Eraser, RefreshCw } from "lucide-react";
import { api } from "@/lib/tauri";
import { useUiStore } from "@/stores/uiStore";
import { cn } from "@/lib/utils";

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
  const [status, setStatus] = useState<"loading" | "ready">("loading");
  const [baseUrl, setBaseUrl] = useState("");

  // Pin the native webview over the host rect (CSS px == window logical px).
  const placeWebview = useCallback(() => {
    const node = hostRef.current;
    if (!node) return;
    if (useUiStore.getState().openModal != null) return; // a modal owns the screen
    const r = node.getBoundingClientRect();
    api.tradetallySetBounds(r.left, r.top, r.width, r.height).catch((e) => {
      // Surface instead of swallowing: a failed create is exactly the "stuck on
      // Chargement…" symptom — the user can then Recharger / Vider le cache.
      console.error("[tradetally] set_bounds failed:", e);
    });
  }, []);

  // Show the configured site host in the toolbar.
  useEffect(() => {
    api.getLocalConfig()
      .then((cfg) => setBaseUrl(cfg?.tradetally?.api_base_url ?? ""))
      .catch(() => {});
  }, []);

  // Backend tells us when the page finished loading → flip the status dot.
  useEffect(() => {
    const unlisten = listen("tradetally-loaded", () => setStatus("ready"));
    return () => { unlisten.then((f) => f()); };
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
      setStatus("loading");
      placeWebview();
    }
  }, [openModal, placeWebview]);

  // Toolbar: hard refresh (destroy + recreate) — robust whether the webview is
  // live or stuck/absent, unlike an in-place reload of a missing webview.
  const handleReload = useCallback(async () => {
    setStatus("loading");
    try {
      await api.tradetallyClose();
    } catch { /* ignore */ }
    placeWebview();
  }, [placeWebview]);

  // Toolbar: nuclear recovery for a corrupted cache. Clears cache + cookies +
  // localStorage, so it logs the user out and resets the dark/light preference —
  // confirm first. Ensures the webview exists before clearing.
  const handleClearData = useCallback(async () => {
    const ok = window.confirm(
      "Vider le cache et les cookies va déconnecter TradeTally et réinitialiser " +
        "la préférence thème clair/sombre. Continuer ?",
    );
    if (!ok) return;
    setStatus("loading");
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

  return (
    <div className="flex flex-1 flex-col overflow-hidden bg-background">
      {/* Toolbar — lives in a strip the native webview never covers. */}
      <div className="flex h-9 shrink-0 items-center gap-2 border-b border-border bg-card px-3 text-xs">
        <span
          className={cn(
            "h-2 w-2 shrink-0 rounded-full",
            status === "ready" ? "bg-emerald-500" : "bg-amber-400 animate-pulse",
          )}
          title={status === "ready" ? "Page chargée" : "Chargement…"}
        />
        <span className="truncate text-muted-foreground">
          {baseUrl || "TradeTally"}
        </span>
        <div className="ml-auto flex items-center gap-1">
          <button
            onClick={handleReload}
            title="Recharger la page (conserve la session)"
            className="flex items-center gap-1 rounded px-2 py-1 text-[11px] text-muted-foreground transition-colors hover:bg-accent/50 hover:text-foreground"
          >
            <RefreshCw className={cn("h-3.5 w-3.5", status === "loading" && "animate-spin")} />
            Recharger
          </button>
          <button
            onClick={handleClearData}
            title="Vider le cache et les cookies, puis recharger (déconnecte TradeTally)"
            className="flex items-center gap-1 rounded px-2 py-1 text-[11px] text-muted-foreground transition-colors hover:bg-red-900/30 hover:text-red-400"
          >
            <Eraser className="h-3.5 w-3.5" />
            Vider cache &amp; cookies
          </button>
        </div>
      </div>

      {/* Webview host: the native webview is pinned over this rect. The
          placeholder shows through only until the webview paints on top (or if
          it failed to appear — then use the toolbar above to recover). */}
      <div ref={hostRef} className="relative flex-1 overflow-hidden">
        <div className="flex h-full w-full flex-col items-center justify-center gap-2 text-sm text-muted-foreground">
          <span>Chargement de TradeTally…</span>
          <span className="text-xs text-muted-foreground/60">
            Bloqué ? Utilisez « Recharger » ou « Vider cache &amp; cookies » ci-dessus.
          </span>
        </div>
      </div>
    </div>
  );
}
