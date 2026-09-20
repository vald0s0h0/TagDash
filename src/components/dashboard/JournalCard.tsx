import { useEffect } from "react";
import { ArrowRight } from "lucide-react";
import { FrostLabel } from "./frosted";
import { useDiaryEntry, useSaveDiary } from "./useDashboard";
import { MicDictate } from "@/components/MicDictate";
import { useJournalStore, journalDayKey } from "@/stores/journalStore";

// Same glass effect as the cards: near-transparent fill, 14.2px blur, 0.04 border.
const fieldClass =
  "w-full rounded-[9px] border border-white/[0.04] bg-white/[0.01] px-3 py-2.5 font-body text-[13px] text-white outline-none backdrop-blur-[14.2px] transition-colors placeholder:text-white/35 focus:border-white/20";

/** Card 02 · FORM — Journal du jour. A free title + body kept (and persisted) all
 *  day long. The SEND button only pushes the current text to TradeTally (create-or-
 *  update for the day) — it never clears the fields, so you can keep editing and
 *  re-send across tab switches and restarts. The draft auto-resets the next day at
 *  midnight ET. The send is queued + retried, so it can't be lost.
 *
 *  TradeTally is the source of truth for the entry, so the card reads it back on
 *  every open and adopts it — an entry written on another machine, or edited in
 *  TradeTally itself, shows up here. Unsent local edits always win over the fetch
 *  (see `dirty` in the journal store), so this can't overwrite what you are typing. */
export function JournalCard() {
  const title = useJournalStore((s) => s.title);
  const content = useJournalStore((s) => s.content);
  const dirty = useJournalStore((s) => s.dirty);
  const setTitle = useJournalStore((s) => s.setTitle);
  const setContent = useJournalStore((s) => s.setContent);
  const rollover = useJournalStore((s) => s.rollover);
  const hydrate = useJournalStore((s) => s.hydrate);
  const save = useSaveDiary();
  const remote = useDiaryEntry();

  // Roll the draft over at ET midnight — on open, and (if left open) on the minute.
  useEffect(() => {
    rollover();
    const id = setInterval(() => rollover(journalDayKey()), 60_000);
    return () => clearInterval(id);
  }, [rollover]);

  // Adopt TradeTally's entry for the day once it arrives.
  const entry = remote.data;
  useEffect(() => {
    if (!entry) return;
    hydrate(entry.entry_date, entry.title, entry.content);
  }, [entry, hydrate]);

  const empty = !title.trim() && !content.trim();

  function onSend() {
    if (empty) return;
    // Push to the API; deliberately keep the text in place for further edits.
    save.mutate({ title: title.trim(), content: content.trim() });
  }

  // One line about where the text on screen stands relative to TradeTally. The
  // send states win (they're the most recent action), then unsent edits, then what
  // the last read of TradeTally said.
  const status = save.isPending
    ? "Envoi…"
    : save.isError
      ? "En file — réessai"
      : dirty
        ? "Non envoyé"
        : save.isSuccess
          ? "Envoyé ✓"
          : remote.isFetching
            ? "Lecture…"
            : remote.isError
              ? "TradeTally injoignable"
              : entry
                ? "TradeTally ✓"
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
        {/* Two small icon buttons, bottom-left: dictate (record & send) + send manual edits. */}
        <div className="absolute bottom-2.5 left-2.5 flex items-center gap-1.5">
          <MicDictate
            mode="diary"
            variant="card"
            title="Dicter une note de journal (enregistrer & envoyer)"
            className="border border-white/[0.12] bg-white/[0.06] text-white/80 hover:bg-white/[0.16] hover:text-white"
          />
          <button
            onClick={onSend}
            disabled={save.isPending || empty}
            title="Envoyer les modifications saisies au clavier"
            className="flex h-7 w-7 items-center justify-center rounded-md border border-white/[0.12] bg-white/[0.06] text-white/80 backdrop-blur transition-colors hover:bg-white/[0.16] hover:text-white disabled:cursor-not-allowed disabled:opacity-40"
          >
            <ArrowRight className="h-4 w-4" />
          </button>
        </div>
      </div>
    </div>
  );
}
