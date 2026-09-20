import { create } from "zustand";
import type { ApexStatus } from "@/types";

/** Live Apex status, mirrored from the backend's `apex-status` event (see
 *  `useApexEvents`). One Apex at a time across every zone, so one status. RAM only:
 *  the backend is the source of truth and re-sends it on launch. */
interface ApexState {
  status: ApexStatus | null;
  setStatus: (status: ApexStatus) => void;
}

export const useApexStore = create<ApexState>((set) => ({
  status: null,
  setStatus: (status) => set({ status }),
}));
