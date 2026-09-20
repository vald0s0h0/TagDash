import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { api } from "@/lib/tauri";
import { useApexStore } from "@/stores/apexStore";
import { useNotificationStore } from "@/stores/notificationStore";
import type { ApexStatus } from "@/types";

/** Mirrors the backend Apex status into `useApexStore` and turns each new notice
 *  (armed, fired, skipped, closed…) into a toast exactly once. Mounted once (App).
 *
 *  The status fetched at launch only seeds the store and the notice watermark — a
 *  notice from before this window opened is history, not news. */
export function useApexEvents(): void {
  useEffect(() => {
    let lastSeq = 0;
    let cancelled = false;
    let unlisten: (() => void) | undefined;

    const apply = (s: ApexStatus, announce: boolean) => {
      useApexStore.getState().setStatus(s);
      const n = s.notice;
      if (!n || n.seq <= lastSeq) return;
      lastSeq = n.seq;
      if (announce) useNotificationStore.getState().notify(n.message, n.level);
    };

    api.getApexStatus()
      .then((s) => { if (!cancelled) apply(s, false); })
      .catch(() => {});

    listen<ApexStatus>("apex-status", (e) => apply(e.payload, true))
      .then((fn) => { if (cancelled) fn(); else unlisten = fn; });

    return () => { cancelled = true; unlisten?.(); };
  }, []);
}
