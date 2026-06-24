import { create } from "zustand";
import { api } from "@/lib/tauri";
import type { SttStatus } from "@/types";

// Frontend mirror of the backend STT pipeline. `status` is refreshed on demand and
// whenever the backend emits `stt-changed` (see useSttEvents). `owner` enforces the
// "one mic at a time" rule across all dictation buttons: whoever starts recording
// claims it, and every other button disables until it's released.

interface SttState {
  status: SttStatus | null;
  /** Stable id of the component currently holding the mic (null = free). */
  owner: string | null;
  refresh: () => Promise<void>;
  claim: (id: string) => boolean;
  release: (id: string) => void;
}

export const useSttStore = create<SttState>((set, get) => ({
  status: null,
  owner: null,
  refresh: async () => {
    try {
      set({ status: await api.sttStatus() });
    } catch {
      /* backend not ready / disabled — leave previous status */
    }
  },
  // Claim the mic for `id`. Returns false if someone else holds it.
  claim: (id) => {
    const { owner } = get();
    if (owner && owner !== id) return false;
    set({ owner: id });
    return true;
  },
  release: (id) => {
    if (get().owner === id) set({ owner: null });
  },
}));

/** True when another component (not `id`) is recording. */
export function micBusyFor(id: string): boolean {
  const owner = useSttStore.getState().owner;
  return owner !== null && owner !== id;
}
