// Where the persisted stores keep their state: the personal database, through the
// backend (`ui_state`, see `local_db/schema.rs`) — not the webview's localStorage.
//
// localStorage belongs to one machine AND one build flavour (`tauri dev` and the
// released app are two origins, so two unrelated copies), and nothing in it can
// travel. In the personal database the dashboard layout, the hotkeys, the gamepad
// layout, the chart theme… are part of the data pack like everything else.
//
// zustand's `persist` reads its storage synchronously while the store module is
// being evaluated. So the whole state is fetched ONCE, before any store exists
// (`loadUserState`, awaited in main.tsx ahead of the app's imports), into a map the
// stores then read from synchronously. Writes update the map at once and reach the
// backend a moment later, coalesced per store.

import { invoke } from "@tauri-apps/api/core";
import { createJSONStorage, type StateStorage } from "zustand/middleware";

const isTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

/** Set once this origin's localStorage has been carried into the database. */
const ADOPTED_FLAG = "tagdash-user-storage-adopted";
/** Every persisted store is named `tagdash-…`. */
const STORE_PREFIX = "tagdash-";
/** A drag or a slider writes on every frame; the backend gets the last value. */
const WRITE_DELAY_MS = 250;

const state = new Map<string, string>();
const pending = new Map<string, number>();
let loaded = false;
/** Backend unreachable at load: this session reads and writes localStorage. */
let local = !isTauri;

function push(key: string): void {
  pending.delete(key);
  const value = state.get(key);
  const call =
    value === undefined
      ? invoke("remove_ui_state", { key })
      : invoke("set_ui_state", { key, value });
  call.catch((e) => console.error(`[tagdash] réglage d'interface non enregistré (${key})`, e));
}

function schedule(key: string): void {
  const timer = pending.get(key);
  if (timer !== undefined) window.clearTimeout(timer);
  pending.set(key, window.setTimeout(() => push(key), WRITE_DELAY_MS));
}

/** Send what is still waiting — the window is going away. */
function flush(): void {
  for (const [key, timer] of pending) {
    window.clearTimeout(timer);
    push(key);
  }
}

/** One time per origin: what this build had in localStorage becomes the database's,
 *  for every store the database does not hold yet. localStorage is left as it is —
 *  it is simply never read again. */
function adoptLocalStorage(): void {
  try {
    if (localStorage.getItem(ADOPTED_FLAG) !== null) return;
    for (let i = 0; i < localStorage.length; i++) {
      const key = localStorage.key(i);
      if (!key || key === ADOPTED_FLAG || !key.startsWith(STORE_PREFIX) || state.has(key)) continue;
      const value = localStorage.getItem(key);
      if (value === null) continue;
      state.set(key, value);
      push(key);
    }
    localStorage.setItem(ADOPTED_FLAG, new Date().toISOString());
  } catch {
    /* localStorage unavailable: nothing to adopt */
  }
}

/** Fetch every persisted store's state. MUST resolve before a module that creates
 *  a persisted store is imported — see main.tsx. */
export async function loadUserState(): Promise<void> {
  if (loaded) return;
  if (isTauri) {
    try {
      const remote = await invoke<Record<string, string>>("get_ui_state");
      for (const [key, value] of Object.entries(remote)) state.set(key, value);
      adoptLocalStorage();
      window.addEventListener("pagehide", flush);
    } catch (e) {
      console.error("[tagdash] réglages d'interface indisponibles — repli sur localStorage", e);
      local = true;
    }
  }
  loaded = true;
}

/** Has this store ever been saved? (A brand-new user has no dashboard layout yet.) */
export function hasUserState(key: string): boolean {
  return local ? localStorage.getItem(key) !== null : state.has(key);
}

const stateStorage: StateStorage = {
  getItem: (key) => {
    if (local) return localStorage.getItem(key);
    if (!loaded) {
      // A store created before the state arrived would start from its defaults and
      // write them over what is saved. Refuse to guess: read nothing, write nothing.
      console.error(`[tagdash] ${key} lu avant loadUserState() — ordre d'import à corriger`);
    }
    return state.get(key) ?? null;
  },
  setItem: (key, value) => {
    if (local) return localStorage.setItem(key, value);
    if (!loaded) return;
    if (state.get(key) === value) return;
    state.set(key, value);
    schedule(key);
  },
  removeItem: (key) => {
    if (local) return localStorage.removeItem(key);
    if (!loaded) return;
    state.delete(key);
    schedule(key);
  },
};

/** `storage` option of every persisted store. */
export const userStorage = <S,>() => createJSONStorage<S>(() => stateStorage);
