import { create } from "zustand";
import type { DipStatus } from "@/types";

/** Live Dip status, mirrored from the backend's `dip-status` event (see
 *  `useDipEvents`). One band at a time across every zone, so one status. RAM only:
 *  the backend is the source of truth and re-sends it on launch. */
interface DipState {
  status: DipStatus | null;
  setStatus: (status: DipStatus) => void;
}

export const useDipStore = create<DipState>((set) => ({
  status: null,
  setStatus: (status) => set({ status }),
}));
