import { useEffect, useState } from "react";
import { CheckCircle2, XCircle, X } from "lucide-react";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { Label } from "@/components/ui/label";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import { Button } from "@/components/ui/button";
import { Separator } from "@/components/ui/separator";
import { Badge } from "@/components/ui/badge";
import { useLocalConfig, useUpdateLocalConfig } from "@/queries/useLocalConfig";
import { useSecretsStatus } from "@/queries/useSecretsStatus";
import { useStrategies, useSetStrategyEnabled, useSetStrategyRisk } from "@/queries/useScanner";
import {
  useHotkeyStore, HOTKEY_ACTIONS, HOTKEY_GROUPS, bindingLabel, bindingFromEvent,
  setRecordingActive, type Binding, type HotkeyActionDef, type HotkeyGroup,
} from "@/stores/hotkeyStore";
import { cn } from "@/lib/utils";
import type { AppConfig, AttentionMode, Session } from "@/types";

const SESSION_LABELS: Record<Session, string> = {
  premarket:  "Premarket",
  pre_open:   "Pre-open",
  open:       "Open",
  afterhours: "Afterhours",
};

const ATTENTION_OPTIONS: { value: AttentionMode; label: string }[] = [
  { value: "off",       label: "Désactivé" },
  { value: "premarket", label: "Prémarket seulement" },
  { value: "open",      label: "Open seulement (après 9h30)" },
  { value: "both",      label: "Prémarket + Open" },
];

/** Native select for an attention-cue schedule (off / premarket / open / both). */
function AttentionSelect({
  value,
  onChange,
}: {
  value: AttentionMode;
  onChange: (v: AttentionMode) => void;
}) {
  return (
    <select
      value={value}
      onChange={(e) => onChange(e.target.value as AttentionMode)}
      className="h-7 shrink-0 rounded-md border border-border bg-background px-2 text-xs"
    >
      {ATTENTION_OPTIONS.map((o) => (
        <option key={o.value} value={o.value}>
          {o.label}
        </option>
      ))}
    </select>
  );
}

interface Props {
  open: boolean;
  onClose: () => void;
}

function SecretRow({ label, configured }: { label: string; configured: boolean }) {
  return (
    <div className="flex items-center justify-between py-1.5">
      <span className="text-sm">{label}</span>
      {configured ? (
        <span className="flex items-center gap-1.5 text-xs text-emerald-400">
          <CheckCircle2 className="h-3.5 w-3.5" /> configured
        </span>
      ) : (
        <span className="flex items-center gap-1.5 text-xs text-muted-foreground">
          <XCircle className="h-3.5 w-3.5" /> not set
        </span>
      )}
    </div>
  );
}

function Field({
  label,
  value,
  onChange,
  type = "text",
}: {
  label: string;
  value: string | number;
  onChange: (v: string) => void;
  type?: string;
}) {
  return (
    <div className="grid grid-cols-2 items-center gap-4">
      <Label className="text-right text-xs text-muted-foreground">{label}</Label>
      <Input
        type={type}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        className="h-7 text-xs"
      />
    </div>
  );
}

/** Editable $-risk-per-trade field for a strategy. Commits on blur or Enter so
 *  the change takes effect immediately on the next order. */
function RiskInput({
  value,
  disabled,
  onCommit,
}: {
  value: number;
  disabled?: boolean;
  onCommit: (risk: number) => void;
}) {
  const [text, setText] = useState(String(value));
  // Re-seed when the upstream value changes (e.g. after a refetch).
  useEffect(() => { setText(String(value)); }, [value]);

  function commit() {
    const n = parseFloat(text);
    if (Number.isFinite(n) && n >= 0 && n !== value) onCommit(n);
    else setText(String(value));
  }

  return (
    <div className="flex items-center gap-1">
      <span className="text-[10px] text-muted-foreground">Risque $</span>
      <Input
        type="number"
        min={0}
        step="1"
        value={text}
        disabled={disabled}
        onChange={(e) => setText(e.target.value)}
        onBlur={commit}
        onKeyDown={(e) => {
          if (e.key === "Enter") (e.target as HTMLInputElement).blur();
        }}
        className="h-6 w-16 text-xs tabular-nums"
      />
    </div>
  );
}

const HOTKEY_GROUP_LABELS: Record<HotkeyGroup, string> = {
  Toolbar:    "Barre d'outils",
  Ordres:     "Ordres",
  Analyse:    "Analyse IA",
  Timeframes: "Timeframes (pane de gauche)",
};

/** One bindable action row: shows the current chord and records a new one (the
 *  next key/mouse-button press while armed). Mouse buttons capture the extra
 *  buttons of a multi-button mouse (left/right are reserved). */
function HotkeyRow({ action }: { action: HotkeyActionDef }) {
  const binding      = useHotkeyStore((s) => s.bindings[action.id]);
  const setBinding   = useHotkeyStore((s) => s.setBinding);
  const clearBinding = useHotkeyStore((s) => s.clearBinding);
  const [recording, setRecording] = useState(false);

  useEffect(() => {
    if (!recording) return;
    setRecordingActive(true);

    function commit(b: Binding) { setBinding(action.id, b); setRecording(false); }
    function onKey(e: KeyboardEvent) {
      e.preventDefault(); e.stopPropagation();
      if (e.code === "Escape") { setRecording(false); return; }
      const b = bindingFromEvent(e); // null for a lone modifier → keep waiting
      if (b) commit(b);
    }
    function onMouse(e: MouseEvent) {
      const b = bindingFromEvent(e); // null for left/right click → ignored
      if (b) { e.preventDefault(); e.stopPropagation(); commit(b); }
    }
    function onAux(e: MouseEvent) { e.preventDefault(); }

    window.addEventListener("keydown", onKey, true);
    window.addEventListener("mousedown", onMouse, true);
    window.addEventListener("auxclick", onAux, true);
    return () => {
      setRecordingActive(false);
      window.removeEventListener("keydown", onKey, true);
      window.removeEventListener("mousedown", onMouse, true);
      window.removeEventListener("auxclick", onAux, true);
    };
  }, [recording, action.id, setBinding]);

  return (
    <div className="flex items-center justify-between gap-2 py-0.5">
      <span className="text-xs">{action.label}</span>
      <div className="flex items-center gap-1.5">
        <button
          onClick={() => setRecording((r) => !r)}
          className={cn(
            "min-w-[7.5rem] rounded border px-2 py-1 text-center text-[11px] font-mono transition-colors",
            recording
              ? "animate-pulse border-blue-500 text-blue-300"
              : binding
              ? "border-border text-foreground/80 hover:bg-accent"
              : "border-dashed border-border/60 text-muted-foreground/50 hover:bg-accent",
          )}
        >
          {recording ? "Appuyez…" : binding ? bindingLabel(binding) : "non assigné"}
        </button>
        <button
          onClick={() => clearBinding(action.id)}
          disabled={!binding || recording}
          className="text-muted-foreground hover:text-red-400 disabled:opacity-20"
          title="Effacer"
        >
          <X className="h-3 w-3" />
        </button>
      </div>
    </div>
  );
}

export function SettingsModal({ open, onClose }: Props) {
  const { data: config } = useLocalConfig();
  const { data: secrets } = useSecretsStatus();
  const { data: strategies = [] } = useStrategies();
  const setStrategyEnabled = useSetStrategyEnabled();
  const setStrategyRisk = useSetStrategyRisk();
  const update = useUpdateLocalConfig();

  const [draft, setDraft] = useState<AppConfig | null>(null);
  const [tagInput, setTagInput] = useState("");

  useEffect(() => {
    if (config) setDraft(structuredClone(config));
  }, [config]);

  if (!draft) return null;

  const tags = draft.journal?.tags ?? [];
  const addTag = (raw: string) => {
    const t = raw.trim().toLowerCase();
    if (t && !tags.includes(t)) set("journal", "tags", [...tags, t]);
    setTagInput("");
  };
  const removeTag = (t: string) =>
    set("journal", "tags", tags.filter((x) => x !== t));

  function set<K extends keyof AppConfig>(
    section: K,
    key: keyof AppConfig[K],
    value: unknown
  ) {
    setDraft((prev) => {
      if (!prev) return prev;
      return {
        ...prev,
        [section]: { ...(prev[section] as object), [key]: value },
      };
    });
  }

  function save() {
    if (!draft) return;
    update.mutate(draft, { onSuccess: onClose });
  }

  return (
    <Dialog open={open} onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="max-w-xl">
        <DialogHeader>
          <DialogTitle>Settings</DialogTitle>
        </DialogHeader>

        <Tabs defaultValue="trading" className="mt-2">
          <TabsList className="w-full">
            <TabsTrigger value="trading" className="flex-1 text-xs">Trading</TabsTrigger>
            <TabsTrigger value="strategies" className="flex-1 text-xs">Stratégies</TabsTrigger>
            <TabsTrigger value="hotkeys" className="flex-1 text-xs">Hotkeys</TabsTrigger>
            <TabsTrigger value="notifs" className="flex-1 text-xs">Notifs</TabsTrigger>
            <TabsTrigger value="latency" className="flex-1 text-xs">Latency</TabsTrigger>
            <TabsTrigger value="tags" className="flex-1 text-xs">Tags</TabsTrigger>
            <TabsTrigger value="tradetally" className="flex-1 text-xs">TradeTally</TabsTrigger>
            <TabsTrigger value="secrets" className="flex-1 text-xs">API Keys</TabsTrigger>
          </TabsList>

          {/* ── Trading ── */}
          <TabsContent value="trading" className="mt-4 space-y-3">
            <Field
              label="Default broker"
              value={draft.trading.default_broker}
              onChange={(v) => set("trading", "default_broker", v)}
            />
            <Field
              label="Default account"
              value={draft.trading.default_account}
              onChange={(v) => set("trading", "default_account", v)}
            />
            <Field
              label="Commission ($)"
              value={draft.trading.default_commission}
              type="number"
              onChange={(v) => set("trading", "default_commission", parseFloat(v) || 0)}
            />
            <Field
              label="Fees ($)"
              value={draft.trading.default_fees}
              type="number"
              onChange={(v) => set("trading", "default_fees", parseFloat(v) || 0)}
            />
            <Field
              label="Max position size"
              value={draft.trading.max_position_size}
              type="number"
              onChange={(v) => set("trading", "max_position_size", parseInt(v) || 0)}
            />
          </TabsContent>

          {/* ── Strategies (runtime on/off, persisted, no code change) ── */}
          <TabsContent value="strategies" className="mt-4 space-y-2">
            <p className="text-xs text-muted-foreground">
              Active/désactive une stratégie et règle le risque $ par trade. Effet
              immédiat, conservé au relancement — pas besoin de toucher au code.
            </p>
            <div className="max-h-72 space-y-1.5 overflow-y-auto pr-1">
              {strategies.length === 0 && (
                <span className="text-xs text-muted-foreground/60">Aucune stratégie.</span>
              )}
              {strategies.map((s) => (
                <div
                  key={s.id}
                  className="flex items-center justify-between rounded-md border border-border px-3 py-2"
                >
                  <div className="min-w-0">
                    <div className="flex items-center gap-2">
                      <span className="truncate text-sm font-medium">{s.name}</span>
                      <span className="rounded bg-muted px-1 py-0.5 text-[9px] tabular-nums text-muted-foreground">
                        P{s.priority}
                      </span>
                    </div>
                    <div className="mt-1 flex flex-wrap gap-1">
                      {s.sessions.map((sess) => (
                        <span
                          key={sess}
                          className="rounded bg-blue-900/40 px-1.5 py-0.5 text-[9px] text-blue-300"
                        >
                          {SESSION_LABELS[sess] ?? sess}
                        </span>
                      ))}
                    </div>
                  </div>
                  <div className="flex shrink-0 items-center gap-3">
                    <RiskInput
                      value={s.max_risk_dollars}
                      disabled={setStrategyRisk.isPending}
                      onCommit={(risk) => setStrategyRisk.mutate({ id: s.id, risk })}
                    />
                    <Switch
                      checked={s.enabled}
                      disabled={setStrategyEnabled.isPending}
                      onCheckedChange={(v) =>
                        setStrategyEnabled.mutate({ id: s.id, enabled: v })
                      }
                    />
                  </div>
                </div>
              ))}
            </div>
          </TabsContent>

          {/* ── Hotkeys (keyboard chords / extra mouse buttons → hovered zone) ── */}
          <TabsContent value="hotkeys" className="mt-4 space-y-3">
            <p className="text-xs text-muted-foreground">
              Assigne une touche, une combinaison clavier ou un bouton de souris
              (boutons latéraux d'une souris multi-boutons) à chaque commande.
              Clique sur le champ puis appuie sur la touche/bouton voulu&nbsp;;
              <kbd className="mx-1 rounded bg-muted px-1 text-[10px]">Échap</kbd>
              annule. Le raccourci agit sur la zone <strong>survolée par la
              souris</strong> (son pane de gauche), sinon sur la zone active. Clic
              gauche/droit réservés.
            </p>
            <div className="max-h-80 space-y-3 overflow-y-auto pr-1">
              {HOTKEY_GROUPS.map((group) => (
                <div key={group}>
                  <div className="mb-0.5 text-[10px] font-semibold uppercase tracking-wide text-muted-foreground/60">
                    {HOTKEY_GROUP_LABELS[group]}
                  </div>
                  <div className="rounded-md border border-border px-3 py-1.5">
                    {HOTKEY_ACTIONS.filter((a) => a.group === group).map((a) => (
                      <HotkeyRow key={a.id} action={a} />
                    ))}
                  </div>
                </div>
              ))}
            </div>
          </TabsContent>

          {/* ── Notifications (native OS desktop alerts) ── */}
          <TabsContent value="notifs" className="mt-4 space-y-3">
            <div className="flex items-center justify-between rounded-md border border-border px-3 py-2.5">
              <div className="min-w-0 pr-4">
                <div className="text-sm font-medium">Alertes système</div>
                <p className="mt-0.5 text-xs text-muted-foreground">
                  Envoie une notification Windows / macOS à chaque alerte du scanner,
                  quelle que soit la stratégie et l'onglet actif.
                </p>
              </div>
              <Switch
                checked={draft.ui.desktop_alerts}
                onCheckedChange={(v) => set("ui", "desktop_alerts", v)}
              />
            </div>

            <div className="flex items-center justify-between gap-4 rounded-md border border-border px-3 py-2.5">
              <div className="min-w-0">
                <div className="text-sm font-medium">Flash blanc à l'écran</div>
                <p className="mt-0.5 text-xs text-muted-foreground">
                  Un flash blanc plein écran (500&nbsp;ms) à chaque alerte — visible
                  même si TagDash est caché derrière d'autres fenêtres.
                </p>
              </div>
              <AttentionSelect
                value={draft.ui.flash_alerts}
                onChange={(v) => set("ui", "flash_alerts", v)}
              />
            </div>

            <div className="flex items-center justify-between gap-4 rounded-md border border-border px-3 py-2.5">
              <div className="min-w-0">
                <div className="text-sm font-medium">Forcer au premier plan</div>
                <p className="mt-0.5 text-xs text-muted-foreground">
                  Ramène la fenêtre TagDash au premier plan et fait clignoter son
                  icône dans la barre des tâches à chaque alerte.
                </p>
              </div>
              <AttentionSelect
                value={draft.ui.foreground_alerts}
                onChange={(v) => set("ui", "foreground_alerts", v)}
              />
            </div>

            <p className="text-[11px] leading-relaxed text-muted-foreground">
              Réglages côté système&nbsp;:
              <br />
              <span className="text-foreground">Windows</span> — autorise les
              notifications pour TagDash (Paramètres → Système → Notifications) et
              désactive l'Assistant de concentration / «&nbsp;Ne pas déranger&nbsp;».
              En mode dev les toasts peuvent ne pas s'afficher&nbsp;; elles
              fonctionnent une fois l'app installée.
              <br />
              <span className="text-foreground">macOS</span> — à la première
              activation, autorise les notifications dans la fenêtre système (ou
              Réglages → Notifications → TagDash).
            </p>
          </TabsContent>

          {/* ── Latency ── */}
          <TabsContent value="latency" className="mt-4 space-y-3">
            <Field
              label="Warn threshold (ms)"
              value={draft.latency.warn_ms}
              type="number"
              onChange={(v) => set("latency", "warn_ms", parseInt(v) || 0)}
            />
            <Field
              label="Critical threshold (ms)"
              value={draft.latency.critical_ms}
              type="number"
              onChange={(v) => set("latency", "critical_ms", parseInt(v) || 0)}
            />
            <Separator />
            <div className="grid grid-cols-2 items-center gap-4">
              <Label className="text-right text-xs text-muted-foreground">
                Alpaca feed
              </Label>
              <Input
                value={draft.alpaca.feed}
                onChange={(e) => set("alpaca", "feed", e.target.value)}
                className="h-7 text-xs"
              />
            </div>
            <div className="grid grid-cols-2 items-center gap-4">
              <Label className="text-right text-xs text-muted-foreground">
                Use news feed
              </Label>
              <Switch
                checked={draft.alpaca.use_news}
                onCheckedChange={(v) => set("alpaca", "use_news", v)}
              />
            </div>
          </TabsContent>

          {/* ── Tags (user-defined journal tags) ── */}
          <TabsContent value="tags" className="mt-4 space-y-3">
            <p className="text-xs text-muted-foreground">
              Your journal tags. These appear as suggestions when tagging a trade.
            </p>
            <div className="flex flex-wrap gap-1.5">
              {tags.length === 0 && (
                <span className="text-xs text-muted-foreground/60">No tags yet.</span>
              )}
              {tags.map((t) => (
                <span
                  key={t}
                  className="flex items-center gap-1 rounded bg-blue-900/40 px-2 py-0.5 text-[11px] text-blue-300"
                >
                  {t}
                  <button
                    onClick={() => removeTag(t)}
                    className="text-blue-400/60 hover:text-blue-300"
                  >
                    <X className="h-3 w-3" />
                  </button>
                </span>
              ))}
            </div>
            <Input
              value={tagInput}
              placeholder="Add a tag and press Enter…"
              onChange={(e) => setTagInput(e.target.value)}
              onKeyDown={(e) => {
                if ((e.key === "Enter" || e.key === ",") && tagInput.trim()) {
                  e.preventDefault();
                  addTag(tagInput);
                }
              }}
              className="h-7 text-xs"
            />
          </TabsContent>

          {/* ── TradeTally ── */}
          <TabsContent value="tradetally" className="mt-4 space-y-3">
            <Field
              label="API base URL"
              value={draft.tradetally.api_base_url}
              onChange={(v) => set("tradetally", "api_base_url", v)}
            />
          </TabsContent>

          {/* ── API Keys (status only) ── */}
          <TabsContent value="secrets" className="mt-4 space-y-1">
            {/* Data source: live API vs offline flat files (same toggle as in
                Gestion Flat Files). Persisted in tagdash.toml. */}
            <div className="mb-3 flex items-center justify-between gap-4 rounded-md border border-border px-3 py-2.5">
              <div className="min-w-0">
                <div className="text-sm font-medium">Source de données</div>
                <p className="mt-0.5 text-xs text-muted-foreground">
                  {(draft.data_source?.mode ?? "api") === "flat_files"
                    ? "Flat files — pas de temps réel, Market Replay uniquement (ouvert par défaut)."
                    : "API Alpaca — données temps réel."}
                </p>
              </div>
              <div className="flex shrink-0 overflow-hidden rounded-md border border-border">
                {(["api", "flat_files"] as const).map((m) => (
                  <button
                    key={m}
                    onClick={() => set("data_source", "mode", m)}
                    className={cn(
                      "px-3 py-1.5 text-xs transition-colors",
                      (draft.data_source?.mode ?? "api") === m
                        ? "bg-accent text-foreground"
                        : "text-muted-foreground hover:bg-accent/50",
                    )}
                  >
                    {m === "api" ? "API" : "Flat files"}
                  </button>
                ))}
              </div>
            </div>
            <Separator className="mb-3" />
            <p className="mb-3 text-xs text-muted-foreground">
              Keys are stored in{" "}
              <code className="rounded bg-muted px-1 py-0.5 text-[11px]">
                tagdash.secrets.toml
              </code>{" "}
              in your app config directory. Edit that file to set them.
            </p>
            <Separator className="mb-3" />
            <SecretRow
              label="Alpaca key"
              configured={secrets?.alpaca_key ?? false}
            />
            <SecretRow
              label="Alpaca secret"
              configured={secrets?.alpaca_secret ?? false}
            />
            <SecretRow
              label="Massive API key (float)"
              configured={secrets?.massive_api_key ?? false}
            />
            <SecretRow
              label="sec-api.io key (country · industry)"
              configured={secrets?.sec_api_key ?? false}
            />
            <SecretRow
              label="FMP API key (legacy/fallback)"
              configured={secrets?.fmp_api_key ?? false}
            />
            <SecretRow
              label="Claude API key"
              configured={secrets?.claude_api_key ?? false}
            />
            <SecretRow
              label="Deepseek API key (micro_pullback news/dilution)"
              configured={secrets?.deepseek_api_key ?? false}
            />
            <SecretRow
              label="TradeTally token"
              configured={secrets?.tradetally_token ?? false}
            />
          </TabsContent>
        </Tabs>

        <div className="mt-4 flex justify-end gap-2">
          <Button variant="ghost" size="sm" onClick={onClose}>
            Cancel
          </Button>
          <Button
            size="sm"
            onClick={save}
            disabled={update.isPending}
          >
            {update.isPending ? "Saving…" : "Save"}
          </Button>
        </div>
      </DialogContent>
    </Dialog>
  );
}
