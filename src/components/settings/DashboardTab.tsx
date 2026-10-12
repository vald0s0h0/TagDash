// Settings → Interface → Tableau de bord. Everything that makes the dashboard the
// user's own, in one place: the daily backgrounds, the mood images, the short and
// long phrases (edited here instead of in a text file), and each card's visibility
// and surface. Applied at once, like the other self-contained tabs. All of it is
// personal data — it travels in the data pack (Réglages ▸ Mes données).

import { useEffect, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { FolderOpen, RotateCcw } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import { CARD_DEFS } from "@/components/dashboard/cards";
import { api } from "@/lib/tauri";
import { cn } from "@/lib/utils";
import { useDashboardStore, type Surface } from "@/stores/dashboardStore";
import type { DashboardAssets } from "@/types";

const SURFACES: { value: Surface; label: string; hint: string }[] = [
  { value: "glass", label: "Glass", hint: "Verre dépoli (flou d'arrière-plan)" },
  { value: "heavy", label: "Heavy", hint: "Fond plein, contrasté" },
  { value: "invisible", label: "Transparent", hint: "Aucun fond : le contenu seul sur la photo" },
];

/** Phrases a file holds: non-empty lines that are not `#` comments. */
function phraseCount(text: string): number {
  return text.split("\n").filter((l) => l.trim() && !l.trim().startsWith("#")).length;
}

export function DashboardTab() {
  const qc = useQueryClient();
  const [assets, setAssets] = useState<DashboardAssets | null>(null);
  const [short, setShort] = useState("");
  const [long, setLong] = useState("");
  const [error, setError] = useState<string | null>(null);

  const layout = useDashboardStore((s) => s.layout);
  const toggleVisible = useDashboardStore((s) => s.toggleVisible);
  const setSurface = useDashboardStore((s) => s.setSurface);
  const resetLayout = useDashboardStore((s) => s.resetLayout);
  const [confirmReset, setConfirmReset] = useState(false);

  const load = () =>
    api.getDashboardAssets().then(
      (a) => {
        setAssets(a);
        setShort(a.short_phrases);
        setLong(a.long_phrases);
      },
      (e) => setError(String(e)),
    );

  useEffect(() => {
    void load();
  }, []);

  const savePhrases = async (kind: "short" | "long", text: string) => {
    if (!assets || text === (kind === "short" ? assets.short_phrases : assets.long_phrases)) return;
    setError(null);
    try {
      await api.saveMoodPhrases(kind, text);
      setAssets({ ...assets, [kind === "short" ? "short_phrases" : "long_phrases"]: text });
      // The cards draw from these files: let the next dashboard open pick anew.
      qc.invalidateQueries({ queryKey: ["mood"] });
    } catch (e) {
      setError(String(e));
    }
  };

  if (!assets) return <div className="text-xs text-muted-foreground">{error ?? "Chargement…"}</div>;

  return (
    <div className="max-w-2xl space-y-7 text-sm">
      {/* ── Images ── */}
      <section className="grid gap-4 md:grid-cols-2">
        <div className="space-y-2 rounded-md border border-border p-3">
          <Label>Fonds d'écran</Label>
          <p className="text-xs text-muted-foreground">
            {assets.backgrounds} image{assets.backgrounds > 1 ? "s" : ""}. Une par jour, à tour de
            rôle, en plein écran derrière les cartes.
          </p>
          <Button
            variant="outline"
            size="sm"
            onClick={() => api.openBackgroundsFolder().then(load, (e) => setError(String(e)))}
          >
            <FolderOpen /> Ouvrir le dossier
          </Button>
        </div>
        <div className="space-y-2 rounded-md border border-border p-3">
          <Label>Images mood</Label>
          <p className="text-xs text-muted-foreground">
            {assets.mood_images} image{assets.mood_images > 1 ? "s" : ""}. Une tirée au hasard à
            chaque ouverture, pour la carte Inspiration.
          </p>
          <Button
            variant="outline"
            size="sm"
            onClick={() => api.openMoodTarget("images").then(load, (e) => setError(String(e)))}
          >
            <FolderOpen /> Ouvrir le dossier
          </Button>
        </div>
      </section>

      {/* ── Phrases ── */}
      <section className="space-y-4">
        <PhraseEditor
          label="Phrases courtes"
          hint="Carte Inspiration et titre (H1). Une par ligne — ex. « less is more »."
          value={short}
          onChange={setShort}
          onCommit={() => savePhrases("short", short)}
          rows={6}
        />
        <PhraseEditor
          label="Phrases longues"
          hint="Carte Citation. Une par ligne, une cinquantaine de mots."
          value={long}
          onChange={setLong}
          onCommit={() => savePhrases("long", long)}
          rows={8}
        />
      </section>

      {/* ── Cards ── */}
      <section className="space-y-2">
        <div className="flex items-center justify-between">
          <Label>Cartes</Label>
          {confirmReset ? (
            <div className="flex gap-2">
              <Button variant="ghost" size="sm" onClick={() => setConfirmReset(false)}>
                Annuler
              </Button>
              <Button
                variant="destructive"
                size="sm"
                onClick={() => {
                  resetLayout();
                  setConfirmReset(false);
                }}
              >
                Réinitialiser
              </Button>
            </div>
          ) : (
            <Button variant="ghost" size="sm" onClick={() => setConfirmReset(true)}>
              <RotateCcw /> Disposition par défaut
            </Button>
          )}
        </div>
        <p className="text-xs text-muted-foreground">
          Affichage et design de chaque carte. Leur place et leur taille se règlent sur le tableau
          de bord lui-même (menu ▸ Éditer la disposition).
        </p>
        <ul className="divide-y divide-border rounded-md border border-border">
          {CARD_DEFS.map((card) => {
            const slot = layout[card.id];
            return (
              <li key={card.id} className="flex items-center gap-3 px-3 py-1.5">
                <Switch checked={slot?.visible ?? false} onCheckedChange={() => toggleVisible(card.id)} />
                <span className={cn("min-w-0 flex-1 truncate text-xs", !slot?.visible && "text-muted-foreground")}>
                  {card.title}
                </span>
                <div className="flex shrink-0 overflow-hidden rounded-md border border-border">
                  {SURFACES.map((s) => (
                    <button
                      key={s.value}
                      type="button"
                      title={s.hint}
                      onClick={() => setSurface(card.id, s.value)}
                      className={cn(
                        "px-2 py-0.5 text-[11px] text-muted-foreground hover:bg-accent",
                        slot?.surface === s.value && "bg-accent text-foreground",
                      )}
                    >
                      {s.label}
                    </button>
                  ))}
                </div>
              </li>
            );
          })}
        </ul>
      </section>

      {error && <p className="text-xs text-red-400">{error}</p>}
    </div>
  );
}

function PhraseEditor({
  label,
  hint,
  value,
  onChange,
  onCommit,
  rows,
}: {
  label: string;
  hint: string;
  value: string;
  onChange: (text: string) => void;
  onCommit: () => void;
  rows: number;
}) {
  const count = phraseCount(value);
  return (
    <div className="space-y-1">
      <div className="flex items-baseline justify-between">
        <Label>{label}</Label>
        <span className="text-[11px] tabular-nums text-muted-foreground">
          {count} phrase{count > 1 ? "s" : ""}
        </span>
      </div>
      <textarea
        value={value}
        onChange={(e) => onChange(e.target.value)}
        onBlur={onCommit}
        rows={rows}
        spellCheck={false}
        className="w-full resize-y rounded border border-border bg-background px-2 py-1.5 text-xs text-foreground outline-none"
      />
      <p className="text-[11px] text-muted-foreground">
        {hint} Les lignes commençant par # sont ignorées. Enregistré en quittant le champ.
      </p>
    </div>
  );
}
