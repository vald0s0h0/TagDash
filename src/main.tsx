import React from "react";
import ReactDOM from "react-dom/client";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
// NOTE: the tauri-plugin-gamepad polyfill (gilrs) detects the pad but never
// delivers button/axis events on Windows here, so we use the webview's NATIVE
// Gamepad API instead — it works once WebView2's renderer-throttling is disabled
// (see additionalBrowserArgs in tauri.conf.json). The native API needs a button
// pressed while the window is focused to start exposing the pad.
import { FlashOverlay } from "./FlashOverlay";
import { loadUserState } from "./lib/userStorage";
import "./index.css";

// NOTHING that creates a persisted store is imported statically here. Those stores
// read their saved state the instant their module is evaluated, and that state
// lives in the personal database: it is fetched first (`loadUserState`), and the
// app — `App`, `AttentionApp`, the usage tracker — is imported after.

const root = ReactDOM.createRoot(document.getElementById("root")!);

// The backend opens a second window labelled "flash" — the transparent always-on-
// top alert-flash overlay. It renders only the white wash, no providers / no app.
// Reading the label is synchronous + IPC-free (window metadata), so it needs no
// capability; guard for plain-browser dev where Tauri internals are absent.
const isTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
const label = isTauri ? getCurrentWindow().label : "main";
const isFlash = label === "flash";
// A third window labelled "attention": the detached « Attention du marché » screen
// meant for a second monitor. Same bundle, its own root — none of the trading
// workspace (feeds, hotkeys, gamepad) is mounted there.
const isAttention = label === "attention";

async function start() {
  await loadUserState();

  // Usage log: every click / command, in the windows the user acts in (the flash
  // overlay takes no input). Needs the backend, so not in plain-browser dev.
  if (isTauri) {
    const { installUsageTracker } = await import("./lib/usageTracker");
    installUsageTracker(label);
  }

  if (isAttention) {
    const { AttentionApp } = await import("./attention/AttentionApp");
    root.render(
      <React.StrictMode>
        <AttentionApp />
      </React.StrictMode>
    );
    return;
  }

  const { default: App } = await import("./App");
  const queryClient = new QueryClient({
    defaultOptions: { queries: { staleTime: 30_000, refetchOnWindowFocus: false } },
  });
  root.render(
    <React.StrictMode>
      <QueryClientProvider client={queryClient}>
        <App />
      </QueryClientProvider>
    </React.StrictMode>
  );
}

if (isFlash) {
  // The flash overlay owns no store and must paint at once: no state to wait for.
  document.documentElement.style.background = "transparent";
  document.body.style.background = "transparent";
  const el = document.getElementById("root");
  if (el) el.style.background = "transparent";
  root.render(<FlashOverlay />);
} else {
  void start();
}
