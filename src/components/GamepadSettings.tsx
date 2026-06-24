import { useEffect, useRef, useState } from "react";
import { X, RotateCcw, Gamepad2 } from "lucide-react";
import { Switch } from "@/components/ui/switch";
import {
  useGamepadStore, GAMEPAD_ACTIONS, GAMEPAD_GROUPS, DEFAULT_BINDINGS, bindingLabel, actionDef,
  type GamepadActionId, type GamepadActionDef, type GamepadGroup,
} from "@/stores/gamepadStore";
import { setGamepadCapturing } from "@/lib/gamepadBus";
import { pickGamepad } from "@/lib/gamepadActions";
import { cn } from "@/lib/utils";

// Settings ▸ Hotkeys ▸ Xbox. Mirrors the keyboard recorder: each command shows its
// current binding and re-records on the next button press (digital) or stick move
// (analog). Binding the wrong kind of input is rejected with an explanatory
// message. Plus the controller image, a live read-out, sensitivities + inverts.

const GROUP_HINTS: Partial<Record<GamepadGroup, string>> = {
  "Curseur / Ordres": "R2 relâché — le curseur horizontal (stick droit) place l'ordre.",
  "Position armée":   "R2 maintenu — les boutons de façade deviennent les tailles de position.",
  "Axes (analogique)": "Sticks : valeur continue −1…+1. La sensibilité et l'inversion se règlent ci-dessous.",
};

function firstConnectedPad(): Gamepad | null {
  return pickGamepad(navigator.getGamepads ? navigator.getGamepads() : []);
}

// ─── One bindable command row ─────────────────────────────────────────────────

function GamepadRow({
  action, recording, onRecord, onClear,
}: {
  action: GamepadActionDef;
  recording: boolean;
  onRecord: () => void;
  onClear: () => void;
}) {
  const binding = useGamepadStore((s) => s.bindings[action.id]);
  return (
    <div className={cn("flex items-center justify-between gap-2 py-0.5", action.reserved && "opacity-50")}>
      <span className="text-xs">
        {action.label}
        {action.reserved && <span className="ml-1 text-[9px] text-muted-foreground">(réservé)</span>}
      </span>
      <div className="flex items-center gap-1.5">
        <button
          onClick={onRecord}
          disabled={action.reserved}
          className={cn(
            "min-w-[7.5rem] rounded border px-2 py-1 text-center text-[11px] font-mono transition-colors",
            recording
              ? "animate-pulse border-blue-500 text-blue-300"
              : "border-border text-foreground/80 hover:bg-accent disabled:cursor-not-allowed disabled:hover:bg-transparent",
          )}
        >
          {recording
            ? (action.kind === "digital" ? "Appuie sur un bouton…" : "Bouge un stick…")
            : bindingLabel(binding)}
        </button>
        <button
          onClick={onClear}
          disabled={recording}
          className="text-muted-foreground hover:text-red-400 disabled:opacity-20"
          title="Réassigner par défaut"
        >
          <X className="h-3 w-3" />
        </button>
      </div>
    </div>
  );
}

// ─── Sensitivity slider ───────────────────────────────────────────────────────

function SensSlider({ label, value, onChange }: { label: string; value: number; onChange: (v: number) => void }) {
  return (
    <div className="flex items-center gap-3">
      <span className="w-40 shrink-0 text-xs text-muted-foreground">{label}</span>
      <input
        type="range" min={0.2} max={3} step={0.1} value={value}
        onChange={(e) => onChange(Number(e.target.value))}
        className="flex-1 accent-blue-500"
      />
      <span className="w-8 text-right text-xs tabular-nums">{value.toFixed(1)}×</span>
    </div>
  );
}

// ─── Panel ────────────────────────────────────────────────────────────────────

export function GamepadSettings() {
  const enabled       = useGamepadStore((s) => s.enabled);
  const setEnabled    = useGamepadStore((s) => s.setEnabled);
  const setBinding    = useGamepadStore((s) => s.setBinding);
  const resetDefaults = useGamepadStore((s) => s.resetDefaults);
  const leftSens      = useGamepadStore((s) => s.leftSensitivity);
  const rightSens     = useGamepadStore((s) => s.rightSensitivity);
  const setLeftSens   = useGamepadStore((s) => s.setLeftSensitivity);
  const setRightSens  = useGamepadStore((s) => s.setRightSensitivity);
  const invertZoomTime  = useGamepadStore((s) => s.invertZoomTime);
  const invertZoomPrice = useGamepadStore((s) => s.invertZoomPrice);
  const invertCursor    = useGamepadStore((s) => s.invertCursor);
  const setInvertZoomTime  = useGamepadStore((s) => s.setInvertZoomTime);
  const setInvertZoomPrice = useGamepadStore((s) => s.setInvertZoomPrice);
  const setInvertCursor    = useGamepadStore((s) => s.setInvertCursor);

  const [recording, setRecording] = useState<GamepadActionId | null>(null);
  const [reject, setReject] = useState<string | null>(null);
  const [connected, setConnected] = useState(false);
  const [live, setLive] = useState<{ buttons: number[]; axes: number[] }>({ buttons: [], axes: [] });
  const [diag, setDiag] = useState<{ present: number; slots: number; id: string; mapping: string; nb: number; na: number }>(
    { present: 0, slots: 0, id: "", mapping: "", nb: 0, na: 0 },
  );
  const [imgOk, setImgOk] = useState(true);

  // Live read-out (throttled ~8 fps) so the user can see the pad responding.
  useEffect(() => {
    let raf = 0;
    let last = 0;
    const tick = (t: number) => {
      if (t - last > 120) {
        last = t;
        // Raw API state — reveals whether navigator.getGamepads() returns anything
        // at all and which backend (gilrs polyfill ≈ 20 btns / native ≈ 17).
        const raw = navigator.getGamepads ? navigator.getGamepads() : [];
        const any = [...raw].find((p) => p) ?? null;
        const pad = firstConnectedPad();
        setConnected(!!pad);
        setDiag({
          present: [...raw].filter((p) => p).length,
          slots: raw.length,
          id: (pad ?? any)?.id ?? "",
          mapping: (pad ?? any)?.mapping ?? "—",
          nb: (pad ?? any)?.buttons.length ?? 0,
          na: (pad ?? any)?.axes.length ?? 0,
        });
        setLive(pad
          ? { buttons: pad.buttons.flatMap((b, i) => (b.pressed ? [i] : [])), axes: [...pad.axes] }
          : { buttons: [], axes: [] });
      }
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  }, []);

  // Recorder loop — armed while `recording` is set.
  useEffect(() => {
    if (!recording) return;
    setGamepadCapturing(true);
    const def = actionDef(recording);
    let restAxes: number[] | null = null;
    let raf = 0;
    const timeout = window.setTimeout(() => stop(), 10_000); // safety: don't hang armed

    const stop = () => {
      window.clearTimeout(timeout);
      cancelAnimationFrame(raf);
      setRecording(null);
      setGamepadCapturing(false);
    };

    const tick = () => {
      const pad = firstConnectedPad();
      if (pad) {
        if (restAxes === null) restAxes = [...pad.axes];
        const btnIdx = pad.buttons.findIndex((b) => b.pressed || b.value > 0.6);
        let axisIdx = -1;
        for (let i = 0; i < pad.axes.length; i++) {
          if (Math.abs(pad.axes[i] - (restAxes[i] ?? 0)) > 0.6) { axisIdx = i; break; }
        }
        if (def?.kind === "digital") {
          if (btnIdx >= 0) { setBinding(recording, { kind: "button", index: btnIdx }); setReject(null); stop(); return; }
          if (axisIdx >= 0) { setReject("Cet axe renvoie une valeur continue — choisis un bouton."); restAxes = [...pad.axes]; }
        } else {
          if (axisIdx >= 0) { setBinding(recording, { kind: "axis", index: axisIdx }); setReject(null); stop(); return; }
          if (btnIdx >= 0) setReject("Ce bouton ne renvoie pas de valeur continue — choisis un stick.");
        }
      }
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);

    const onKey = (e: KeyboardEvent) => { if (e.key === "Escape") { e.preventDefault(); stop(); } };
    window.addEventListener("keydown", onKey, true);
    return () => { window.clearTimeout(timeout); cancelAnimationFrame(raf); setGamepadCapturing(false); window.removeEventListener("keydown", onKey, true); };
  }, [recording, setBinding]);

  const startRecord = (id: GamepadActionId) => { setReject(null); setRecording((r) => (r === id ? null : id)); };

  return (
    <div className="space-y-3">
      {/* Top: big controller image (left) + status / live test / sensitivity (right) */}
      <div className="flex gap-4">
        {/* Controller reference image (drop file at public/xbox-controller.png). */}
        <div className="w-[44%] shrink-0">
          {imgOk ? (
            <img
              src="/xbox-controller.png"
              alt="Disposition des boutons de la manette Xbox"
              onError={() => setImgOk(false)}
              className="w-full rounded-md border border-border object-contain"
            />
          ) : (
            <div className="flex min-h-[14rem] items-center justify-center rounded-md border border-dashed border-border px-3 py-6 text-center text-[11px] text-muted-foreground">
              Image manette absente — déposer{" "}
              <code className="mx-1 rounded bg-muted px-1">public/xbox-controller.png</code>.
            </div>
          )}
        </div>

        {/* Right column: status · live test · sensitivity */}
        <div className="flex min-w-0 flex-1 flex-col gap-3">
          {/* Enable + status + reset */}
          <div className="flex items-center justify-between rounded-md border border-border px-3 py-2.5">
            <div className="min-w-0">
              <div className="flex items-center gap-2 text-sm font-medium">
                <Gamepad2 className="h-4 w-4 text-muted-foreground" />
                Manette Xbox
                <span className={cn(
                  "rounded px-1.5 py-0.5 text-[9px] font-semibold uppercase",
                  connected ? "bg-emerald-900/50 text-emerald-300" : "bg-zinc-800 text-muted-foreground",
                )}>
                  {connected ? "connectée" : "absente"}
                </span>
              </div>
              <p className="mt-0.5 text-xs text-muted-foreground">
                Active dès qu'une manette est branchée (USB-C) ou appairée (Bluetooth, macOS).
              </p>
            </div>
            <div className="flex items-center gap-3">
              <button
                onClick={resetDefaults}
                title="Réinitialiser les boutons par défaut"
                className="flex items-center gap-1 text-[11px] text-muted-foreground hover:text-foreground"
              >
                <RotateCcw className="h-3 w-3" /> Défauts
              </button>
              <Switch checked={enabled} onCheckedChange={setEnabled} />
            </div>
          </div>

          {/* Live read-out */}
          <div className="rounded-md border border-border px-3 py-2 text-[11px]">
            <div className="mb-1 flex items-center justify-between font-medium text-muted-foreground">
              <span>Test en direct</span>
              <span className="font-mono text-[10px] text-muted-foreground/70">
                API: {diag.present}/{diag.slots} · map={diag.mapping} · {diag.nb}b/{diag.na}a
              </span>
            </div>
            {diag.id && <div className="mb-1 truncate font-mono text-[10px] text-muted-foreground/50" title={diag.id}>{diag.id}</div>}
            <div className="flex flex-wrap items-center gap-2">
              <span className="text-muted-foreground">Boutons :</span>
              {live.buttons.length === 0
                ? <span className="text-muted-foreground/50">—</span>
                : live.buttons.map((i) => (
                    <span key={i} className="rounded bg-blue-900/50 px-1.5 py-0.5 font-mono text-blue-200">{i}</span>
                  ))}
            </div>
            <div className="mt-1.5 grid grid-cols-2 gap-x-4 gap-y-1">
              {/* W3C standard axis indices: 0=LX 1=LY 2=RX 3=RY. */}
              {([[0, "Stick G ↔"], [1, "Stick G ↕"], [2, "Stick D ↔"], [3, "Stick D ↕"]] as const).map(([idx, label]) => {
                const v = live.axes[idx] ?? 0;
                return (
                  <div key={idx} className="flex items-center gap-2">
                    <span className="w-20 shrink-0 text-muted-foreground">{label}</span>
                    <div className="relative h-1.5 flex-1 rounded bg-border">
                      <div
                        className="absolute top-0 h-full rounded bg-sky-500"
                        style={{ left: "50%", width: `${Math.abs(v) * 50}%`, transform: v < 0 ? "translateX(-100%)" : "none" }}
                      />
                    </div>
                  </div>
                );
              })}
            </div>
          </div>

          {/* Sensitivity + invert */}
          <div className="space-y-2 rounded-md border border-border px-3 py-2.5">
            <div className="text-[10px] font-semibold uppercase tracking-wide text-muted-foreground/60">Sensibilité des sticks</div>
            <SensSlider label="Stick gauche (zoom)" value={leftSens} onChange={setLeftSens} />
            <SensSlider label="Stick droit (curseur)" value={rightSens} onChange={setRightSens} />
            <div className="flex flex-wrap gap-4 pt-1">
              {([
                ["Inverser zoom horizontal", invertZoomTime, setInvertZoomTime],
                ["Inverser zoom vertical",   invertZoomPrice, setInvertZoomPrice],
                ["Inverser curseur",         invertCursor, setInvertCursor],
              ] as const).map(([label, val, set]) => (
                <label key={label} className="flex items-center gap-2 text-[11px] text-muted-foreground">
                  <Switch checked={val} onCheckedChange={set} />
                  {label}
                </label>
              ))}
            </div>
          </div>
        </div>
      </div>

      {reject && (
        <div className="rounded-md border border-amber-700/50 bg-amber-950/40 px-3 py-1.5 text-[11px] text-amber-200">
          {reject}
        </div>
      )}

      {/* Command rows, grouped */}
      <div className="max-h-72 space-y-3 overflow-y-auto pr-1">
        {GAMEPAD_GROUPS.map((group) => (
          <div key={group}>
            <div className="mb-0.5 text-[10px] font-semibold uppercase tracking-wide text-muted-foreground/60">{group}</div>
            {GROUP_HINTS[group] && (
              <p className="mb-1 text-[10px] leading-tight text-muted-foreground/50">{GROUP_HINTS[group]}</p>
            )}
            <div className="rounded-md border border-border px-3 py-1.5">
              {GAMEPAD_ACTIONS.filter((a) => a.group === group).map((a) => (
                <GamepadRow
                  key={a.id}
                  action={a}
                  recording={recording === a.id}
                  onRecord={() => startRecord(a.id)}
                  onClear={() => setBinding(a.id, DEFAULT_BINDINGS[a.id])}
                />
              ))}
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}
