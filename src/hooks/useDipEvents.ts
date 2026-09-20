import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { useQueryClient } from "@tanstack/react-query";
import { api } from "@/lib/tauri";
import { useDipStore } from "@/stores/dipStore";
import { useNotificationStore } from "@/stores/notificationStore";
import type { DipStatus } from "@/types";

/** Mirrors the backend Dip status into `useDipStore` and turns each new notice
 *  (band placed, resized, attack lapsed, fired, closed…) into a toast exactly once.
 *  Mounted once (App).
 *
 *  The status fetched at launch only seeds the store and the notice watermark — a
 *  notice from before this window opened is history, not news.
 *
 *  The thickness hotkeys write the tuned thickness back as the default for the next
 *  band, so a status carrying a new one also makes the cached options stale. */
export function useDipEvents(): void {
  const qc = useQueryClient();
  useEffect(() => {
    let lastSeq = 0;
    let thickness: number | null = null;
    let cancelled = false;
    let unlisten: (() => void) | undefined;

    const apply = (s: DipStatus, announce: boolean) => {
      useDipStore.getState().setStatus(s);
      if (s.thickness_cents != null && s.thickness_cents !== thickness) {
        if (thickness != null) qc.invalidateQueries({ queryKey: ["dip-options"] });
        thickness = s.thickness_cents;
      }
      const n = s.notice;
      if (!n || n.seq <= lastSeq) return;
      lastSeq = n.seq;
      if (announce) useNotificationStore.getState().notify(n.message, n.level);
    };

    api.getDipStatus()
      .then((s) => { if (!cancelled) apply(s, false); })
      .catch(() => {});

    listen<DipStatus>("dip-status", (e) => apply(e.payload, true))
      .then((fn) => { if (cancelled) fn(); else unlisten = fn; });

    return () => { cancelled = true; unlisten?.(); };
  }, [qc]);
}
