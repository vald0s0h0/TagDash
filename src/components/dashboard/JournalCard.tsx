import { useEffect } from "react";
import { FrostLabel } from "./frosted";
import { useSaveDiary } from "./useDashboard";
import { useJournalStore, journalDayKey } from "@/stores/journalStore";

// Same glass effect as the cards: near-transparent fill, 14.2px blur, 0.04 border.
const fieldClass =
  "w-full rounded-[9px] border border-white/[0.04] bg-white/[0.01] px-3 py-2.5 font-body text-[13px] text-white outline-none backdrop-blur-[14.2px] transition-colors placeholder:text-white/35 focus:border-white/20";

/** Card 02 · FORM — Journal du jour. A free title + body kept (and persisted) all
 *  day long. The SEND button only pushes the current text to TradeTally (create-or-
 *  update for the day) — it never clears the fields, so you can keep editing and
 *  re-send across tab switches and restarts. The draft auto-resets the next day at
 *  midnight ET. The send is queued + retried, so it can't be lost. */
export function JournalCard() {
  const title = useJournalStore((s) => s.title);
  const content = useJournalStore((s) => s.content);
  const setTitle = useJournalStore((s) => s.setTitle);
  const setContent = useJournalStore((s) => s.setContent);
  const rollover = useJournalStore((s) => s.rollover);
  const save = useSaveDiary();

  // Roll the draft over at ET midnight — on open, and (if left open) on the minute.
  useEffect(() => {
    rollover();
    const id = setInterval(() => rollover(journalDayKey()), 60_000);
    return () => clearInterval(id);
  }, [rollover]);

  const empty = !title.trim() && !content.trim();

  function onSend() {
    if (empty) return;
    // Push to the API; deliberately keep the text in place for further edits.
    save.mutate({ title: title.trim(), content: content.trim() });
  }

  const status = save.isPending
    ? "Envoi…"
    : save.isSuccess
      ? "À jour ✓"
      : save.isError
        ? "En file — réessai"
        : null;

  return (
    <div className="flex h-full w-full flex-col gap-[11px] p-[22px]">
      <div className="flex items-baseline justify-between">
        <FrostLabel>Journal du jour</FrostLabel>
        {status && (
          <span className="font-spacemono text-[10px] uppercase tracking-[0.10em] text-white/45">
            {status}
          </span>
        )}
      </div>

      <input
        placeholder="Titre du jour"
        value={title}
        onChange={(e) => setTitle(e.target.value)}
        className={fieldClass}
      />

      <div className="relative min-h-0 flex-1">
        <textarea
          placeholder="Notes, état d'esprit, plan…"
          value={content}
          onChange={(e) => setContent(e.target.value)}
          className={`${fieldClass} h-full resize-none pb-11`}
        />
        <button
          onClick={onSend}
          disabled={save.isPending || empty}
          className="absolute bottom-2.5 right-2.5 rounded-[7px] border border-white/[0.22] bg-white/[0.18] px-[13px] py-[7px] font-spacemono text-[11px] uppercase tracking-[0.06em] text-white backdrop-blur transition-colors hover:bg-white/[0.26] disabled:cursor-not-allowed disabled:opacity-40"
        >
          Envoyer →
        </button>
      </div>
    </div>
  );
}
