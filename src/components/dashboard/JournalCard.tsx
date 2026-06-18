import { useState } from "react";
import { Input } from "@/components/ui/input";
import { Button } from "@/components/ui/button";
import { useSaveDiary } from "./useDashboard";

/** Diary card: a free title + body that becomes today's TradeTally diary entry
 *  (create-or-update for the current ET day). Distinct from per-trade notes. The
 *  send is queued + retried by the background worker, so it can't be lost. */
export function JournalCard() {
  const [title, setTitle] = useState("");
  const [content, setContent] = useState("");
  const save = useSaveDiary();

  const empty = !title.trim() && !content.trim();

  function onSend() {
    if (empty) return;
    save.mutate(
      { title: title.trim(), content: content.trim() },
      {
        onSuccess: () => {
          setTitle("");
          setContent("");
        },
      }
    );
  }

  return (
    <div className="flex h-full flex-col gap-2">
      <Input
        placeholder="Titre du jour"
        value={title}
        onChange={(e) => setTitle(e.target.value)}
        className="border-white/10 bg-white/5"
      />
      <textarea
        placeholder="Notes, état d'esprit, plan…"
        value={content}
        onChange={(e) => setContent(e.target.value)}
        className="min-h-0 flex-1 resize-none rounded-md border border-white/10 bg-white/5 px-3 py-2 text-sm outline-none transition-colors placeholder:text-muted-foreground focus-visible:ring-1 focus-visible:ring-ring"
      />
      <div className="flex items-center justify-between">
        <span className="text-[11px] text-foreground/50">
          {save.isPending
            ? "Envoi…"
            : save.isSuccess
              ? "Ajouté au journal TradeTally ✓"
              : save.isError
                ? "En file — réessai auto"
                : "Diary du jour"}
        </span>
        <Button size="sm" onClick={onSend} disabled={save.isPending || empty}>
          Envoyer
        </Button>
      </div>
    </div>
  );
}
