import { useState, useEffect } from "react";
import { NotebookPen, Tag, X } from "lucide-react";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { api } from "@/lib/tauri";
import { cn } from "@/lib/utils";

interface Props {
  open:     boolean;
  onClose:  () => void;
  tradeId:  string;
  symbol:   string;
}

export function JournalModal({ open, onClose, tradeId, symbol }: Props) {
  const [notes,      setNotes]      = useState("");
  const [confidence, setConfidence] = useState<number>(5);
  const [tags,       setTags]       = useState<string[]>([]);
  const [tagInput,   setTagInput]   = useState("");
  const [cachedTags, setCachedTags] = useState<string[]>([]);
  const [saving,     setSaving]     = useState(false);
  const [savedAt,    setSavedAt]    = useState<string | null>(null);

  // Load cached tags + existing journal entry on open
  useEffect(() => {
    if (!open) return;
    api.getJournalTags().then(setCachedTags).catch(() => {});
    api.getJournalEntry(tradeId).then((entry) => {
      if (entry) {
        setNotes(entry.notes);
        setConfidence(entry.confidence ?? 5);
        setTags(entry.tags);
        setSavedAt(entry.updated_at);
      } else {
        setNotes("");
        setConfidence(5);
        setTags([]);
        setSavedAt(null);
      }
    }).catch(() => {});
  }, [open, tradeId]);

  const addTag = (tag: string) => {
    const t = tag.trim().toLowerCase();
    if (t && !tags.includes(t)) setTags((prev) => [...prev, t]);
    setTagInput("");
  };

  const removeTag = (tag: string) => setTags((prev) => prev.filter((t) => t !== tag));

  const handleSave = async () => {
    setSaving(true);
    try {
      await api.saveJournalEntry(tradeId, symbol, notes, confidence, tags);
      setSavedAt(new Date().toISOString());
      // Save succeeded → close the modal (the entry is persisted out-of-band).
      onClose();
    } catch (e) {
      console.error("saveJournalEntry failed:", e);
    } finally {
      setSaving(false);
    }
  };

  const suggestedTags = cachedTags.filter(
    (t) => !tags.includes(t) && t.includes(tagInput.toLowerCase())
  ).slice(0, 8);

  return (
    <Dialog open={open} onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="max-w-lg">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <NotebookPen className="h-4 w-4 text-muted-foreground" />
            Journal — {symbol}
          </DialogTitle>
        </DialogHeader>

        <p className="font-mono text-[10px] text-muted-foreground truncate">{tradeId}</p>

        {/* Notes */}
        <div className="space-y-1">
          <label className="text-[11px] uppercase tracking-wide text-muted-foreground">
            Notes
          </label>
          <textarea
            className="w-full resize-none rounded border border-border bg-background px-2.5 py-2 text-sm text-foreground placeholder-muted-foreground/40 outline-none focus:border-blue-500/60 focus:ring-1 focus:ring-blue-500/20"
            rows={4}
            placeholder="Describe the setup, execution, mistakes…"
            value={notes}
            onChange={(e) => setNotes(e.target.value)}
          />
        </div>

        {/* Confidence slider */}
        <div className="space-y-2">
          <div className="flex items-center justify-between">
            <label className="text-[11px] uppercase tracking-wide text-muted-foreground">
              Confidence
            </label>
            <span className={cn(
              "text-sm font-bold tabular-nums",
              confidence <= 3 ? "text-red-400"
              : confidence <= 6 ? "text-amber-400"
              : "text-emerald-400"
            )}>
              {confidence} / 10
            </span>
          </div>
          <input
            type="range"
            min={1}
            max={10}
            step={1}
            value={confidence}
            onChange={(e) => setConfidence(Number(e.target.value))}
            className="w-full accent-blue-500"
          />
          <div className="flex justify-between text-[9px] text-muted-foreground/50">
            <span>Incertain</span>
            <span>Très confiant</span>
          </div>
        </div>

        {/* Tags */}
        <div className="space-y-2">
          <label className="text-[11px] uppercase tracking-wide text-muted-foreground">
            Tags
          </label>

          {/* Selected tags */}
          {tags.length > 0 && (
            <div className="flex flex-wrap gap-1">
              {tags.map((tag) => (
                <span
                  key={tag}
                  className="flex items-center gap-1 rounded bg-blue-900/40 px-1.5 py-0.5 text-[11px] text-blue-300"
                >
                  {tag}
                  <button
                    onClick={() => removeTag(tag)}
                    className="text-blue-400/60 hover:text-blue-300"
                  >
                    <X className="h-2.5 w-2.5" />
                  </button>
                </span>
              ))}
            </div>
          )}

          {/* Tag input + suggestions */}
          <div className="relative">
            <div className="flex items-center gap-1 rounded border border-border bg-background px-2.5 py-1.5">
              <Tag className="h-3 w-3 shrink-0 text-muted-foreground/50" />
              <input
                className="flex-1 bg-transparent text-xs text-foreground placeholder-muted-foreground/40 outline-none"
                placeholder="Ajouter un tag…"
                value={tagInput}
                onChange={(e) => setTagInput(e.target.value)}
                onKeyDown={(e) => {
                  if ((e.key === "Enter" || e.key === ",") && tagInput.trim()) {
                    e.preventDefault();
                    addTag(tagInput);
                  }
                }}
              />
            </div>

            {/* Suggestions dropdown */}
            {tagInput && suggestedTags.length > 0 && (
              <div className="absolute left-0 right-0 top-full z-10 mt-1 rounded border border-border bg-popover shadow-md">
                {suggestedTags.map((t) => (
                  <button
                    key={t}
                    onClick={() => addTag(t)}
                    className="flex w-full items-center px-2.5 py-1.5 text-left text-xs hover:bg-accent"
                  >
                    {t}
                  </button>
                ))}
              </div>
            )}
          </div>

          {/* Cached tag chips (not yet selected) */}
          {!tagInput && cachedTags.filter((t) => !tags.includes(t)).length > 0 && (
            <div className="flex flex-wrap gap-1">
              {cachedTags.filter((t) => !tags.includes(t)).slice(0, 12).map((t) => (
                <button
                  key={t}
                  onClick={() => addTag(t)}
                  className="rounded border border-border/60 px-1.5 py-0.5 text-[11px] text-muted-foreground hover:border-blue-500/50 hover:text-blue-400"
                >
                  {t}
                </button>
              ))}
            </div>
          )}
        </div>

        {/* Footer */}
        <div className="flex items-center justify-between pt-1">
          {savedAt ? (
            <p className="text-[10px] text-muted-foreground">
              Sauvegardé · TT sync pending
            </p>
          ) : (
            <span />
          )}
          <div className="flex gap-2">
            <button
              onClick={onClose}
              className="rounded border border-border px-3 py-1.5 text-xs text-muted-foreground hover:bg-accent"
            >
              Fermer
            </button>
            <button
              onClick={handleSave}
              disabled={saving}
              className="rounded bg-blue-600 px-3 py-1.5 text-xs font-medium text-white hover:bg-blue-500 disabled:opacity-50"
            >
              {saving ? "Sauvegarde…" : "Sauvegarder"}
            </button>
          </div>
        </div>
      </DialogContent>
    </Dialog>
  );
}
