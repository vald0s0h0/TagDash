import React from "react";
import ReactDOM from "react-dom/client";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import App from "./App";
import { FlashOverlay } from "./FlashOverlay";
import "./index.css";

const root = ReactDOM.createRoot(document.getElementById("root")!);

// The backend opens a second window labelled "flash" — the transparent always-on-
// top alert-flash overlay. It renders only the white wash, no providers / no app.
// Reading the label is synchronous + IPC-free (window metadata), so it needs no
// capability; guard for plain-browser dev where Tauri internals are absent.
const isTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
const isFlash = isTauri && getCurrentWindow().label === "flash";

if (isFlash) {
  document.documentElement.style.background = "transparent";
  document.body.style.background = "transparent";
  const el = document.getElementById("root");
  if (el) el.style.background = "transparent";
  root.render(<FlashOverlay />);
} else {
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
