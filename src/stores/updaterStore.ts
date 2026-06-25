// Automatic updates (Tauri official updater). Two entry points share one flow:
//
//  • run()      — launch auto-update, the first step of the startup pipeline. On a
//                 deployed build it checks the GitHub Releases `latest.json`
//                 endpoint; if a newer signed version exists it is downloaded,
//                 installed and the app relaunches into it. Skipped in dev. Runs
//                 once per launch and never blocks startup (errors are captured).
//  • checkNow() / installNow() — manual check / forced install, driven from the
//                 "Mise à jour" modal (LeftRail menu). These ignore the launch
//                 guard and the dev short-circuit so the user can verify the
//                 current version, force an update and read the debug log at will.
//
// Every step appends a timestamped line to `logs`, surfaced in the modal so a
// non-technical user can copy/paste what happened when an update misbehaves.

import { create } from "zustand";
import { check, type Update } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import { getVersion } from "@tauri-apps/api/app";

export type UpdateStatus =
  | "idle"        // not started yet
  | "disabled"    // skipped (development build)
  | "checking"    // querying the update endpoint
  | "uptodate"    // no newer version
  | "available"   // a newer version was found
  | "downloading" // fetching the new bundle
  | "installing"  // applying it (relaunch imminent)
  | "error";      // check/download failed (startup continues)

// The `Update` handle returned by check() is not serializable, so it lives here
// (module scope) rather than in the store — retained between a manual check and a
// manual install so "Installer" doesn't have to re-query.
let pendingUpdate: Update | null = null;

interface UpdaterState {
  status: UpdateStatus;
  /** Newer version string when one is available. */
  version: string | null;
  /** The currently installed app version (from tauri.conf.json via getVersion). */
  currentVersion: string | null;
  /** Download progress, 0..1. */
  progress: number;
  error: string | null;
  /** Timestamped debug lines, shown (and copyable) in the update modal. */
  logs: string[];
  /** Guards the launch auto-run against running more than once per launch. */
  started: boolean;
  /** True while a check/download/install is in flight (guards re-entry). */
  busy: boolean;
  /** Read the installed version into `currentVersion`. */
  loadCurrentVersion: () => Promise<void>;
  /** Launch flow: check → download → install → relaunch (idempotent, dev-skipped). */
  run: () => Promise<void>;
  /** Manual: query the endpoint and report, without installing. */
  checkNow: () => Promise<void>;
  /** Manual/forced: download + install the pending update (checks first if needed). */
  installNow: () => Promise<void>;
}

export const useUpdaterStore = create<UpdaterState>((set, get) => {
  const log = (msg: string) =>
    set((s) => ({
      logs: [...s.logs, `[${new Date().toLocaleTimeString()}] ${msg}`],
    }));

  // Shared download → install → relaunch. On success the app relaunches, so this
  // never returns normally in that case.
  const downloadAndInstall = async (update: Update) => {
    let total = 0;
    let downloaded = 0;
    set({ status: "available", version: update.version });
    await update.downloadAndInstall((event) => {
      switch (event.event) {
        case "Started":
          total = event.data.contentLength ?? 0;
          set({ status: "downloading", progress: 0 });
          log(
            `Téléchargement démarré (${
              total ? `${(total / 1_048_576).toFixed(1)} Mo` : "taille inconnue"
            }).`
          );
          break;
        case "Progress":
          downloaded += event.data.chunkLength;
          set({ progress: total > 0 ? downloaded / total : 0 });
          break;
        case "Finished":
          set({ status: "installing", progress: 1 });
          log("Téléchargement terminé — installation…");
          break;
      }
    });
    log("Installée. Redémarrage de l'application…");
    await relaunch();
  };

  return {
    status: "idle",
    version: null,
    currentVersion: null,
    progress: 0,
    error: null,
    logs: [],
    started: false,
    busy: false,

    loadCurrentVersion: async () => {
      try {
        const v = await getVersion();
        set({ currentVersion: v });
      } catch (e) {
        log(`Impossible de lire la version installée : ${String(e)}`);
      }
    },

    run: async () => {
      if (get().started) return;
      set({ started: true });
      // Fire-and-forget so the version shows up in the modal even at launch.
      void get().loadCurrentVersion();

      // Never auto-run the updater in dev — it would relaunch the dev build.
      if (import.meta.env.DEV) {
        set({ status: "disabled" });
        log("Build de développement — mise à jour automatique ignorée.");
        return;
      }

      set({ busy: true });
      try {
        set({ status: "checking", error: null });
        log("Recherche d'une mise à jour au lancement…");
        const update = await check();
        if (!update) {
          set({ status: "uptodate" });
          log("Application déjà à jour.");
          return;
        }
        log(`Mise à jour ${update.version} disponible.`);
        await downloadAndInstall(update);
      } catch (e) {
        // A missing endpoint / signature / network error must not block startup.
        set({ status: "error", error: String(e) });
        log(`Erreur : ${String(e)}`);
      } finally {
        set({ busy: false });
      }
    },

    checkNow: async () => {
      if (get().busy) return;
      await get().loadCurrentVersion();
      // The updater plugin is only registered in release builds (see lib.rs:
      // cfg(all(desktop, not(debug_assertions)))), so check() would fail in dev.
      if (import.meta.env.DEV) {
        set({ status: "disabled" });
        log("Build de développement : plugin updater non chargé, vérification impossible.");
        return;
      }
      pendingUpdate = null;
      set({ busy: true, error: null, status: "checking", progress: 0 });
      log("Vérification manuelle des mises à jour…");
      try {
        const update = await check();
        if (!update) {
          set({ status: "uptodate" });
          log(
            `Aucune mise à jour : version ${get().currentVersion ?? "?"} déjà à jour.`
          );
          return;
        }
        pendingUpdate = update;
        set({ status: "available", version: update.version });
        log(
          `Mise à jour disponible : ${update.version} (installée : ${
            get().currentVersion ?? "?"
          }).`
        );
      } catch (e) {
        set({ status: "error", error: String(e) });
        log(`Erreur lors de la vérification : ${String(e)}`);
      } finally {
        set({ busy: false });
      }
    },

    installNow: async () => {
      if (get().busy) return;
      if (import.meta.env.DEV) {
        set({ status: "disabled" });
        log("Build de développement : installation impossible (plugin updater non chargé).");
        return;
      }
      // Cold open (no prior check) → query first.
      if (!pendingUpdate) await get().checkNow();
      if (!pendingUpdate) {
        log("Rien à installer.");
        return;
      }
      set({ busy: true, error: null });
      log("Installation forcée de la mise à jour…");
      try {
        await downloadAndInstall(pendingUpdate);
        // Relaunch happens inside downloadAndInstall — unreachable on success.
      } catch (e) {
        set({ status: "error", error: String(e), busy: false });
        log(`Erreur lors de l'installation : ${String(e)}`);
      }
    },
  };
});

/** True while an update is being checked, downloaded or installed — used to keep
 *  the startup modal open so its progress stays visible until relaunch. */
export function isUpdateInProgress(status: UpdateStatus): boolean {
  return (
    status === "checking" ||
    status === "available" ||
    status === "downloading" ||
    status === "installing"
  );
}
