import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";

// Content of the always-on-top "flash" window. The window is transparent +
// click-through and stays up permanently (so it never steals focus); it's invisible
// until the backend emits `tagdash://flash` on a new scanner alert, at which point a
// full-screen white div pulses once (~500 ms) — visible even when other apps cover
// TagDash. Re-keying the div on each event restarts the CSS animation so rapid
// alerts each flash cleanly.
const FLASH_EVENT = "tagdash://flash";

export function FlashOverlay() {
  const [pulse, setPulse] = useState(0);

  useEffect(() => {
    const un = listen(FLASH_EVENT, () => setPulse((p) => p + 1));
    return () => { un.then((f) => f()); };
  }, []);

  return (
    <>
      <style>{`
        @keyframes tagdash-flash {
          0%   { opacity: 0; }
          20%  { opacity: 0.8; }
          100% { opacity: 0; }
        }
        .tagdash-flash-pulse { animation: tagdash-flash 500ms ease-out; }
      `}</style>
      <div
        key={pulse}
        className={pulse > 0 ? "tagdash-flash-pulse" : undefined}
        style={{
          position: "fixed",
          inset: 0,
          background: "#ffffff",
          opacity: 0,
          pointerEvents: "none",
        }}
      />
    </>
  );
}
