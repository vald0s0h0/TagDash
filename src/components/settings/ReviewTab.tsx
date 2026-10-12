// Settings → Trading → Revue. Which trades go to review (TradeTally tags or setup —
// TagDash files the strategy as the TradeTally setup), which scanner alerts open a
// recording session (the same list), and the auto voice recorder. Applied at once,
// like the other self-contained tabs.

import { useEffect, useState } from "react";
import { X } from "lucide-react";
import { Label } from "@/components/ui/label";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import { api } from "@/lib/tauri";
import { useAutoVoiceStore } from "@/stores/autoVoiceStore";
import { invalidate as invalidateReview, prefetch as prefetchReview } from "@/components/review/queue";
import type { ReviewConfig } from "@/types";

export function ReviewTab() {
  const [cfg, setCfg] = useState<ReviewConfig | null>(null);
  const [tagInput, setTagInput] = useState("");
  const [error, setError] = useState<string | null>(null);
  const autoEnabled = useAutoVoiceStore((s) => s.enabled);

  useEffect(() => {
    api.getReviewConfig().then(setCfg, (e) => setError(String(e)));
  }, []);

  const save = async (next: ReviewConfig) => {
    const tagsChanged = !!cfg && next.tags.join("\n") !== cfg.tags.join("\n");
    setCfg(next);
    setError(null);
    try {
      await api.setReviewConfig(next);
      // Other tags → the trade prepared for the Revue tab may no longer belong.
      if (tagsChanged) {
        invalidateReview();
        prefetchReview();
      }
    } catch (e) {
      setError(String(e));
    }
  };

  if (!cfg) return <div className="text-xs text-muted-foreground">{error ?? "Chargement…"}</div>;

  const addTag = () => {
    const t = tagInput.trim();
    if (!t || cfg.tags.some((x) => x.toLowerCase() === t.toLowerCase())) {
      setTagInput("");
      return;
    }
    save({ ...cfg, tags: [...cfg.tags, t] });
    setTagInput("");
  };

  return (
    <div className="max-w-xl space-y-6 text-sm">
      <section className="space-y-2">
        <Label>Tags passés en revue</Label>
        <p className="text-xs text-muted-foreground">
          Un trade TradeTally est tiré au sort dans l'onglet Revue quand un de ses tags ou son setup
          figure ici (sans tenir compte des majuscules). Les alertes de ces stratégies ouvrent aussi la
          session d'enregistrement du ticker (tape, spread, voix).
        </p>
        <div className="flex flex-wrap gap-1.5">
          {cfg.tags.map((t) => (
            <span key={t} className="flex items-center gap-1 rounded bg-accent px-2 py-0.5 text-xs">
              {t}
              <button
                type="button"
                onClick={() => save({ ...cfg, tags: cfg.tags.filter((x) => x !== t) })}
                className="text-muted-foreground hover:text-red-400"
                title="Retirer"
              >
                <X className="h-3 w-3" />
              </button>
            </span>
          ))}
        </div>
        <div className="flex gap-2">
          <Input
            value={tagInput}
            onChange={(e) => setTagInput(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && addTag()}
            placeholder="Ajouter un tag ou un setup (Entrée)"
            className="h-8 text-xs"
          />
        </div>
      </section>

      <section className="flex items-start justify-between gap-4">
        <div>
          <Label>Enregistrement vocal automatique</Label>
          <p className="text-xs text-muted-foreground">
            Le micro démarre dès l'apparition d'un ticker revu ; « Libérer » l'arrête et envoie la note.
            Aussi réglable au survol du bouton micro d'un graphique.
          </p>
        </div>
        <Switch
          checked={autoEnabled}
          onCheckedChange={async (on) => {
            await useAutoVoiceStore.getState().setEnabled(on);
            setCfg((c) => (c ? { ...c, auto_voice: on } : c));
          }}
        />
      </section>

      <section className="grid grid-cols-2 gap-4">
        <div className="space-y-1">
          <Label>Avant l'alerte (min)</Label>
          <Input
            type="number"
            min={0}
            max={60}
            value={cfg.pre_window_min}
            onChange={(e) => save({ ...cfg, pre_window_min: Math.max(0, Number(e.target.value) || 0) })}
            className="h-8 text-xs"
          />
          <p className="text-[11px] text-muted-foreground">Tape gardé avant l'ouverture de la session.</p>
        </div>
        <div className="space-y-1">
          <Label>Après la sortie (min)</Label>
          <Input
            type="number"
            min={0}
            max={120}
            value={cfg.post_window_min}
            onChange={(e) => save({ ...cfg, post_window_min: Math.max(0, Number(e.target.value) || 0) })}
            className="h-8 text-xs"
          />
          <p className="text-[11px] text-muted-foreground">Fenêtre de la reconstitution SIP après la sortie.</p>
        </div>
      </section>

      {error && <p className="text-xs text-red-400">{error}</p>}
    </div>
  );
}
