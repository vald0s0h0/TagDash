import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { api } from "@/lib/tauri";
import { useSttStore } from "@/stores/sttStore";
import { useJournalStore } from "@/stores/journalStore";
import type { SttDiaryResult } from "@/types";

// Global STT event wiring, mounted once at the app root. Two listeners:
//   • `stt-changed`      → refresh the status mirror (queue, worker, download…).
//   • `stt-diary-result` → a diary dictée finished: append the cleaned block to the
//     dashboard journal draft (never overwrite) and send it to TradeTally. This lives
//     at the app root (not in the card) so it works even when the dashboard tab is
//     unmounted — "update the card fields when uploaded".
export function useSttEvents() {
  const refresh = useSttStore((s) => s.refresh);

  useEffect(() => {
    refresh();

    const unsubs: Array<Promise<() => void>> = [];

    unsubs.push(listen("stt-changed", () => { refresh(); }));

    unsubs.push(
      listen<SttDiaryResult>("stt-diary-result", (e) => {
        const block = (e.payload.block ?? "").trim();
        if (!block) return;
        const js = useJournalStore.getState();
        js.rollover();
        const cur = useJournalStore.getState();
        const nextContent = cur.content.trim() ? `${cur.content}\n\n${block}` : block;
        cur.setContent(nextContent);
        const title = e.payload.title?.trim();
        if (title && !cur.title.trim()) cur.setTitle(title);
        const finalTitle = useJournalStore.getState().title.trim();
        api.saveDiaryEntry(finalTitle, nextContent).catch(() => {});
      }),
    );

    return () => {
      unsubs.forEach((p) => p.then((u) => u()).catch(() => {}));
    };
  }, [refresh]);
}
