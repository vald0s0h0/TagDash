// Settings → Comptes & Système → Mes données.
//
// Personal data lives in ONE folder (`user/`), apart from the re-downloadable cache
// (`cache/`) — see `src-tauri/src/paths.rs`. This page shows what that folder holds
// and moves it between machines as a single file, the data pack: export here,
// import there. Nothing is sent anywhere; the user carries the file.
//
// An import replaces the personal data, so it is staged and applied at the next
// launch (never under a running app), and what it replaces is kept as a backup the
// same page can put back.

import { useCallback, useEffect, useState, type ReactNode } from "react";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { relaunch } from "@tauri-apps/plugin-process";
import {
  Download,
  FolderOpen,
  HardDrive,
  History,
  Loader2,
  RefreshCw,
  Trash2,
  TriangleAlert,
  Upload,
  UserRound,
} from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import { api } from "@/lib/tauri";
import { cn } from "@/lib/utils";
import type {
  DataBackup,
  DataCategory,
  DataCounts,
  DataOverview,
  ExportedPack,
  FoundPack,
  PackManifest,
} from "@/types";

const PACK_EXTENSION = ".tagdashpack";

function formatBytes(bytes: number): string {
  if (bytes <= 0) return "—";
  const units = ["o", "Ko", "Mo", "Go"];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit++;
  }
  const digits = value >= 100 || unit === 0 ? 0 : 1;
  return `${value.toLocaleString("fr-FR", { maximumFractionDigits: digits })} ${units[unit]}`;
}

/** `2026-10-12T10:40:00Z` → `12/10/2026 12:40` on this machine's clock. */
function formatInstant(iso: string): string {
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return iso;
  return date.toLocaleString("fr-FR", { dateStyle: "short", timeStyle: "short" });
}

/** The overview's local `YYYY-MM-DD HH:MM` as a Date. */
function parseLocal(stamp: string | null): Date | null {
  if (!stamp) return null;
  const date = new Date(stamp.replace(" ", "T"));
  return Number.isNaN(date.getTime()) ? null : date;
}

function summary(c: DataCounts): string {
  return `${c.trades} trades · ${c.journal_notes} notes · ${c.review_sessions} sessions de revue · ${c.screenshots} captures`;
}

export function DataTab() {
  const [view, setView] = useState<DataOverview | null>(null);
  const [packs, setPacks] = useState<FoundPack[]>([]);
  const [error, setError] = useState<string | null>(null);
  /** What is running: "export" | "import" | "backup:<name>" | "purge". */
  const [busy, setBusy] = useState<string | null>(null);

  const [withSecrets, setWithSecrets] = useState(false);
  const [exported, setExported] = useState<ExportedPack | null>(null);

  /** The pack the user is about to import (picked, dropped or typed). */
  const [chosen, setChosen] = useState<FoundPack | null>(null);
  const [pathInput, setPathInput] = useState("");
  const [dragging, setDragging] = useState(false);
  const [confirmPurge, setConfirmPurge] = useState(false);

  const refresh = useCallback(async () => {
    try {
      const [overview, found] = await Promise.all([api.userDataOverview(), api.userDataFindPacks()]);
      setView(overview);
      setPacks(found);
    } catch (e) {
      setError(String(e));
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const choose = useCallback(async (path: string) => {
    setError(null);
    try {
      setChosen(await api.userDataInspect(path));
    } catch (e) {
      setChosen(null);
      setError(String(e));
    }
  }, []);

  // A pack dropped anywhere on the window while this page is open.
  useEffect(() => {
    let off: (() => void) | undefined;
    let gone = false;
    getCurrentWebview()
      .onDragDropEvent((event) => {
        const p = event.payload;
        if (p.type === "enter" || p.type === "over") setDragging(true);
        else if (p.type === "leave") setDragging(false);
        else if (p.type === "drop") {
          setDragging(false);
          const pack = p.paths.find((f) => f.toLowerCase().endsWith(PACK_EXTENSION));
          if (pack) void choose(pack);
          else setError(`Dépose un fichier ${PACK_EXTENSION}.`);
        }
      })
      .then((unlisten) => {
        if (gone) unlisten();
        else off = unlisten;
      })
      .catch(() => {});
    return () => {
      gone = true;
      off?.();
    };
  }, [choose]);

  const run = async (what: string, task: () => Promise<void>) => {
    setBusy(what);
    setError(null);
    try {
      await task();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
      void refresh();
    }
  };

  const exportPack = () =>
    run("export", async () => {
      setExported(await api.userDataExport(withSecrets));
    });

  const importChosen = () =>
    chosen &&
    run("import", async () => {
      await api.userDataStageImport(chosen.path);
      setChosen(null);
      setPathInput("");
    });

  if (!view) {
    return <div className="text-xs text-muted-foreground">{error ?? "Chargement…"}</div>;
  }

  const localChange = parseLocal(view.last_change);
  const legacyCount = view.legacy.tables.length + view.legacy.keys;

  return (
    <div className="max-w-3xl space-y-7 text-sm">
      {/* ── What this is ── */}
      <section className="space-y-2">
        <div className="flex items-start justify-between gap-4">
          <div>
            <h3 className="flex items-center gap-2 font-medium">
              <UserRound className="h-4 w-4" /> Mes données
            </h3>
            <p className="mt-1 text-xs text-muted-foreground">
              Tout ce qui t'appartient et qu'aucun fournisseur ne peut te rendre est rangé dans un
              seul dossier, à part du cache. Sur cette machine ({view.machine}) :{" "}
              {summary(view.counts)}.
              {localChange && <> Dernière modification le {formatInstant(localChange.toISOString())}.</>}
            </p>
          </div>
          <Button variant="outline" size="sm" onClick={() => api.userDataOpen("user").catch(() => {})}>
            <FolderOpen /> Ouvrir
          </Button>
        </div>
        <p className="break-all font-mono text-[11px] text-muted-foreground/70">{view.user_dir}</p>
      </section>

      {/* ── A pack is waiting for the restart ── */}
      {view.staged && (
        <StagedNotice
          manifest={view.staged}
          busy={busy !== null}
          onCancel={() => run("import", () => api.userDataCancelImport())}
        />
      )}

      {/* ── Move to another machine ── */}
      <section className="grid gap-4 md:grid-cols-2">
        <div className="space-y-3 rounded-md border border-border p-3">
          <h4 className="flex items-center gap-2 text-xs font-semibold uppercase tracking-wide text-muted-foreground">
            <Download className="h-3.5 w-3.5" /> Exporter
          </h4>
          <p className="text-xs text-muted-foreground">
            Crée un fichier unique ({formatBytes(view.personal_bytes)} avant compression) dans
            Téléchargements. Copie-le sur l'autre machine, puis importe-le là-bas.
          </p>
          <div className="flex items-start justify-between gap-3">
            <div>
              <Label>Inclure les secrets</Label>
              <p className="text-[11px] text-muted-foreground">
                Clés API, identifiants TradeTally, topic ntfy — en clair dans le fichier. Sans eux,
                la machine qui importe garde les siens.
              </p>
            </div>
            <Switch checked={withSecrets} onCheckedChange={setWithSecrets} />
          </div>
          <Button size="sm" onClick={exportPack} disabled={busy !== null}>
            {busy === "export" ? <Loader2 className="animate-spin" /> : <Download />}
            Exporter mes données
          </Button>
          {exported && (
            <p className="break-all text-[11px] text-emerald-400">
              Pack créé ({formatBytes(exported.pack_bytes)}
              {exported.manifest.includes_secrets ? ", secrets inclus" : ", sans secrets"}) :{" "}
              {exported.path}
            </p>
          )}
        </div>

        <div
          className={cn(
            "space-y-3 rounded-md border border-border p-3 transition-colors",
            dragging && "border-primary bg-primary/5",
          )}
        >
          <h4 className="flex items-center gap-2 text-xs font-semibold uppercase tracking-wide text-muted-foreground">
            <Upload className="h-3.5 w-3.5" /> Importer
          </h4>
          <p className="text-xs text-muted-foreground">
            Dépose un fichier {PACK_EXTENSION} sur cette fenêtre, choisis-en un ci-dessous, ou colle
            son chemin. Le pack remplace tes données personnelles ; le cache de cette machine n'est
            pas touché.
          </p>
          {packs.length > 0 && (
            <ul className="space-y-1">
              {packs.map((p) => (
                <li key={p.path}>
                  <button
                    type="button"
                    onClick={() => setChosen(p)}
                    className={cn(
                      "w-full rounded border border-transparent px-2 py-1 text-left text-xs hover:bg-accent",
                      chosen?.path === p.path && "border-border bg-accent",
                    )}
                    title={p.path}
                  >
                    <span className="font-medium">{p.manifest.machine}</span>{" "}
                    <span className="text-muted-foreground">
                      · {formatInstant(p.manifest.created_at)} · {formatBytes(p.pack_bytes)}
                    </span>
                  </button>
                </li>
              ))}
            </ul>
          )}
          <div className="flex gap-2">
            <Input
              value={pathInput}
              onChange={(e) => setPathInput(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && pathInput.trim() && void choose(pathInput)}
              placeholder={`Chemin d'un fichier ${PACK_EXTENSION}`}
              className="h-8 text-xs"
            />
            <Button
              variant="outline"
              size="sm"
              onClick={() => void choose(pathInput)}
              disabled={!pathInput.trim()}
            >
              Lire
            </Button>
          </div>
        </div>
      </section>

      {chosen && (
        <ImportConfirm
          pack={chosen}
          local={view.counts}
          localChange={localChange}
          busy={busy === "import"}
          onCancel={() => setChosen(null)}
          onConfirm={importChosen}
        />
      )}

      {error && (
        <p className="flex items-start gap-2 text-xs text-red-400">
          <TriangleAlert className="mt-0.5 h-3.5 w-3.5 shrink-0" /> {error}
        </p>
      )}

      {/* ── What the personal folder holds ── */}
      <CategoryTable
        title="Contenu du dossier personnel"
        total={view.personal_bytes}
        rows={view.personal}
      />

      {/* ── Backups left by imports ── */}
      {view.backups.length > 0 && (
        <section className="space-y-2">
          <div className="flex items-center justify-between">
            <h4 className="flex items-center gap-2 text-xs font-semibold uppercase tracking-wide text-muted-foreground">
              <History className="h-3.5 w-3.5" /> Sauvegardes
            </h4>
            <button
              type="button"
              className="text-[11px] text-muted-foreground hover:text-foreground"
              onClick={() => api.userDataOpen("backups").catch(() => {})}
            >
              Ouvrir le dossier
            </button>
          </div>
          <p className="text-xs text-muted-foreground">
            Les données qu'un import a remplacées. Restaurer en remet une en place au prochain
            démarrage (les données actuelles deviennent une sauvegarde à leur tour).
          </p>
          <ul className="divide-y divide-border rounded-md border border-border">
            {view.backups.map((b) => (
              <BackupRow
                key={b.name}
                backup={b}
                busy={busy !== null}
                onRestore={() => run(`backup:${b.name}`, () => api.userDataRestoreBackup(b.name))}
                onDelete={() => run(`backup:${b.name}`, () => api.userDataDeleteBackup(b.name))}
              />
            ))}
          </ul>
        </section>
      )}

      {/* ── Cache ── */}
      <section className="space-y-2">
        <CategoryTable
          title="Cache"
          icon={<HardDrive className="h-3.5 w-3.5" />}
          total={view.cache_bytes}
          rows={view.cache}
          note="Données de marché et de référence : tout se retélécharge, rien de tout ça ne va dans un pack."
        />
        <p className="break-all font-mono text-[11px] text-muted-foreground/70">{view.cache_dir}</p>
      </section>

      {/* ── Leftovers of the pre-separation layout ── */}
      {(legacyCount > 0 || view.legacy.unmoved.length > 0 || view.legacy_files.length > 0) && (
        <section className="space-y-2 rounded-md border border-border p-3">
          <h4 className="text-xs font-semibold uppercase tracking-wide text-muted-foreground">
            Avant la séparation
          </h4>
          {legacyCount > 0 && (
            <div className="flex items-start justify-between gap-4">
              <p className="text-xs text-muted-foreground">
                Tes données personnelles ont été copiées hors de la base de marché. Les originaux
                y sont conservés ({view.legacy.tables.length} tables, {view.legacy.rows} lignes) le
                temps que tu vérifies que tout est là. Les supprimer est définitif.
              </p>
              {confirmPurge ? (
                <div className="flex shrink-0 gap-2">
                  <Button variant="ghost" size="sm" onClick={() => setConfirmPurge(false)}>
                    Annuler
                  </Button>
                  <Button
                    variant="destructive"
                    size="sm"
                    disabled={busy !== null}
                    onClick={() =>
                      run("purge", async () => {
                        await api.userDataPurgeLegacy();
                        setConfirmPurge(false);
                      })
                    }
                  >
                    Supprimer définitivement
                  </Button>
                </div>
              ) : (
                <Button variant="outline" size="sm" className="shrink-0" onClick={() => setConfirmPurge(true)}>
                  <Trash2 /> Supprimer les originaux
                </Button>
              )}
            </div>
          )}
          {view.legacy.unmoved.length > 0 && (
            <p className="text-xs text-amber-400">
              Non déplacées (toujours dans la base de marché, et absentes d'un pack) :{" "}
              {view.legacy.unmoved.join(", ")}.
            </p>
          )}
          {view.legacy_files.length > 0 && (
            <p className="text-xs text-amber-400">
              Anciens éléments restés à la racine du dossier de données, ignorés par cette version :{" "}
              {view.legacy_files.join(", ")}.{" "}
              <button
                type="button"
                className="underline"
                onClick={() => api.userDataOpen("root").catch(() => {})}
              >
                Ouvrir
              </button>
            </p>
          )}
        </section>
      )}
    </div>
  );
}

// ─── Pieces ─────────────────────────────────────────────────────────────────────

function CategoryTable({
  title,
  icon,
  total,
  rows,
  note,
}: {
  title: string;
  icon?: ReactNode;
  total: number;
  rows: DataCategory[];
  note?: string;
}) {
  return (
    <section className="space-y-2">
      <div className="flex items-baseline justify-between">
        <h4 className="flex items-center gap-2 text-xs font-semibold uppercase tracking-wide text-muted-foreground">
          {icon} {title}
        </h4>
        <span className="text-xs tabular-nums text-muted-foreground">{formatBytes(total)}</span>
      </div>
      {note && <p className="text-xs text-muted-foreground">{note}</p>}
      <ul className="divide-y divide-border rounded-md border border-border">
        {rows.map((row) => (
          <li key={row.id} className="flex items-center gap-3 px-3 py-1.5">
            <span className="w-44 shrink-0 text-xs">{row.label}</span>
            <span className="min-w-0 flex-1 truncate text-xs text-muted-foreground" title={row.detail}>
              {row.detail}
            </span>
            <span className="w-16 shrink-0 text-right text-xs tabular-nums text-muted-foreground">
              {formatBytes(row.bytes)}
            </span>
            <button
              type="button"
              title="Ouvrir le dossier"
              disabled={!row.openable}
              onClick={() => api.userDataOpen(row.id).catch(() => {})}
              className={cn(
                "shrink-0 text-muted-foreground hover:text-foreground",
                !row.openable && "invisible",
              )}
            >
              <FolderOpen className="h-3.5 w-3.5" />
            </button>
          </li>
        ))}
      </ul>
    </section>
  );
}

/** Pack against this machine, side by side, before one replaces the other. */
function ImportConfirm({
  pack,
  local,
  localChange,
  busy,
  onCancel,
  onConfirm,
}: {
  pack: FoundPack;
  local: DataCounts;
  localChange: Date | null;
  busy: boolean;
  onCancel: () => void;
  onConfirm: () => void;
}) {
  const m = pack.manifest;
  const packDate = new Date(m.created_at);
  // The case the user must not walk into blind: this machine was used AFTER the
  // pack was made, so importing it discards newer work.
  const localIsNewer = localChange !== null && localChange.getTime() > packDate.getTime();
  const rows: [string, number, number][] = [
    ["Trades", m.counts.trades, local.trades],
    ["Notes de journal", m.counts.journal_notes, local.journal_notes],
    ["Sessions de revue", m.counts.review_sessions, local.review_sessions],
    ["Notes vocales", m.counts.voice_notes, local.voice_notes],
    ["Captures", m.counts.screenshots, local.screenshots],
    ["Dessins", m.counts.drawings, local.drawings],
    ["Envois en attente", m.counts.pending_sync, local.pending_sync],
  ];
  return (
    <section className="space-y-3 rounded-md border border-border bg-card/40 p-3">
      <div>
        <h4 className="text-sm font-medium">Remplacer mes données par ce pack ?</h4>
        <p className="break-all text-[11px] text-muted-foreground">{pack.file_name}</p>
      </div>
      <table className="w-full text-xs">
        <thead>
          <tr className="text-left text-muted-foreground">
            <th className="py-1 font-normal" />
            <th className="py-1 font-normal">
              Pack — {m.machine}, {formatInstant(m.created_at)}
            </th>
            <th className="py-1 font-normal">
              Cette machine{localChange && <> — modifiée le {formatInstant(localChange.toISOString())}</>}
            </th>
          </tr>
        </thead>
        <tbody>
          {rows.map(([label, inPack, here]) => (
            <tr key={label} className="border-t border-border">
              <td className="py-1 text-muted-foreground">{label}</td>
              <td className="py-1 tabular-nums">{inPack}</td>
              <td className={cn("py-1 tabular-nums", here > inPack && "text-amber-400")}>{here}</td>
            </tr>
          ))}
        </tbody>
      </table>
      {localIsNewer && (
        <p className="flex items-start gap-2 text-xs text-amber-400">
          <TriangleAlert className="mt-0.5 h-3.5 w-3.5 shrink-0" />
          Cette machine a été utilisée après la création du pack : ce qui y a été fait depuis sera
          remplacé (et gardé en sauvegarde).
        </p>
      )}
      <p className="text-xs text-muted-foreground">
        {m.includes_secrets
          ? "Le pack contient des secrets : ils remplacent ceux de cette machine."
          : "Le pack ne contient pas de secrets : ceux de cette machine sont conservés."}{" "}
        Le remplacement se fait au prochain démarrage ; tes données actuelles sont gardées dans
        Sauvegardes. Après un import, continue sur une seule machine à la fois : deux machines qui
        avancent chacune de leur côté ne se fusionnent pas.
      </p>
      <div className="flex justify-end gap-2">
        <Button variant="ghost" size="sm" onClick={onCancel} disabled={busy}>
          Annuler
        </Button>
        <Button size="sm" onClick={onConfirm} disabled={busy}>
          {busy ? <Loader2 className="animate-spin" /> : <Upload />}
          Remplacer mes données
        </Button>
      </div>
    </section>
  );
}

function StagedNotice({
  manifest,
  busy,
  onCancel,
}: {
  manifest: PackManifest;
  busy: boolean;
  onCancel: () => void;
}) {
  const [failed, setFailed] = useState(false);
  const restart = async () => {
    try {
      await relaunch();
    } catch {
      // Unavailable in some dev contexts: say so rather than leave a dead button.
      setFailed(true);
    }
  };
  return (
    <section className="space-y-2 rounded-md border border-primary/60 bg-primary/5 p-3">
      <h4 className="text-sm font-medium">Un pack attend le redémarrage</h4>
      <p className="text-xs text-muted-foreground">
        Données de {manifest.machine} du {formatInstant(manifest.created_at)} ({summary(manifest.counts)}).
        Elles remplaceront les tiennes au prochain démarrage de TagDash. Rien n'a encore changé.
      </p>
      {failed && (
        <p className="text-xs text-amber-400">
          Redémarrage automatique indisponible ici : ferme puis relance TagDash.
        </p>
      )}
      <div className="flex justify-end gap-2">
        <Button variant="ghost" size="sm" onClick={onCancel} disabled={busy}>
          Annuler l'import
        </Button>
        <Button size="sm" onClick={restart} disabled={busy}>
          <RefreshCw /> Redémarrer maintenant
        </Button>
      </div>
    </section>
  );
}

function BackupRow({
  backup,
  busy,
  onRestore,
  onDelete,
}: {
  backup: DataBackup;
  busy: boolean;
  onRestore: () => void;
  onDelete: () => void;
}) {
  const [confirmDelete, setConfirmDelete] = useState(false);
  return (
    <li className="flex items-center gap-3 px-3 py-1.5">
      <span className="w-32 shrink-0 text-xs tabular-nums">
        {formatInstant(parseLocal(backup.set_aside_at)?.toISOString() ?? backup.set_aside_at)}
      </span>
      <span className="min-w-0 flex-1 truncate text-xs text-muted-foreground" title={summary(backup.counts)}>
        {summary(backup.counts)}
      </span>
      <span className="w-16 shrink-0 text-right text-xs tabular-nums text-muted-foreground">
        {formatBytes(backup.bytes)}
      </span>
      {confirmDelete ? (
        <>
          <Button variant="ghost" size="sm" onClick={() => setConfirmDelete(false)}>
            Annuler
          </Button>
          <Button variant="destructive" size="sm" disabled={busy} onClick={onDelete}>
            Supprimer
          </Button>
        </>
      ) : (
        <>
          <Button variant="outline" size="sm" disabled={busy} onClick={onRestore}>
            Restaurer
          </Button>
          <button
            type="button"
            title="Supprimer cette sauvegarde"
            className="text-muted-foreground hover:text-red-400"
            onClick={() => setConfirmDelete(true)}
          >
            <Trash2 className="h-3.5 w-3.5" />
          </button>
        </>
      )}
    </li>
  );
}
