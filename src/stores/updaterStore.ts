// Automatic updates (Tauri official updater) driven at app launch — the first
// step of the startup pipeline. On a deployed (production) build it checks the
// GitHub Releases `latest.json` endpoint; if a newer signed version exists it is
// downloaded, installed and the app relaunches into it. In development the updater
// is skipped (no signed artifacts / endpoint), and any runtime failure is captured
// as an error status rather than thrown — it never blocks app startup.

import { create } from "zustand";
import { check } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";

export type UpdateStatus =
  | "idle"        // not started yet
  | "disabled"    // skipped (development build)
  | "checking"    // querying the update endpoint
  | "uptodate"    // no newer version
  | "available"   // a newer version was found
  | "downloading" // fetching the new bundle
  | "installing"  // applying it (relaunch imminent)
  | "error";      // check/download failed (startup continues)

interface UpdaterState {
  status: UpdateStatus;
  /** Newer version string when one is available. */
  version: string | null;
  /** Download progress, 0..1. */
  progress: number;
  error: string | null;
  /** Guards against running the flow more than once per launch. */
  started: boolean;
  /** Run the check → download → install → relaunch flow (idempotent). */
  run: () => Promise<void>;
}

export const useUpdaterStore = create<UpdaterState>((set, get) => ({
  status: "idle",
  version: null,
  progress: 0,
  error: null,
  started: false,

  run: async () => {
    if (get().started) return;
    set({ started: true });

    // Never run the updater in dev — it has no signed artifacts or endpoint.
    if (import.meta.env.DEV) {
      set({ status: "disabled" });
      return;
    }

    try {
      set({ status: "checking", error: null });
      const update = await check();
      if (!update) {
        set({ status: "uptodate" });
        return;
      }

      set({ status: "available", version: update.version });

      let total = 0;
      let downloaded = 0;
      await update.downloadAndInstall((event) => {
        switch (event.event) {
          case "Started":
            total = event.data.contentLength ?? 0;
            set({ status: "downloading", progress: 0 });
            break;
          case "Progress":
            downloaded += event.data.chunkLength;
            set({ progress: total > 0 ? downloaded / total : 0 });
            break;
          case "Finished":
            set({ status: "installing", progress: 1 });
            break;
        }
      });

      // New version installed — relaunch into it.
      await relaunch();
    } catch (e) {
      // A missing endpoint / signature / network error must not block startup.
      set({ status: "error", error: String(e) });
    }
  },
}));

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
