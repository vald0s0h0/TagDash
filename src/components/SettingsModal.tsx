import { useEffect, useState, type ReactNode } from "react";
import {
  Bell,
  CheckCircle2,
  Database,
  DownloadCloud,
  Gamepad2,
  Gauge,
  KeyRound,
  Keyboard,
  LineChart,
  ListChecks,
  Mic,
  Newspaper,
  Palette,
  Play,
  Radio,
  Receipt,
  RefreshCw,
  ScrollText,
  Table2,
  Tag,
  X,
  XCircle,
  type LucideIcon,
} from "lucide-react";
import { NOTIF_SOUNDS, playNotifSound } from "@/lib/notifSounds";
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
import { useLocalConfig, useUpdateLocalConfig } from "@/queries/useLocalConfig";
import { useSecretsStatus, useUpdateSecrets } from "@/queries/useSecretsStatus";
import { useStrategies, useSetStrategyEnabled, useSetStrategyRisk } from "@/queries/useScanner";
import {
  useHotkeyStore, HOTKEY_ACTIONS, HOTKEY_GROUPS, bindingLabel, bindingFromEvent,
  setRecordingActive, type Binding, type HotkeyActionDef, type HotkeyGroup,
} from "@/stores/hotkeyStore";
import { GamepadSettings } from "@/components/GamepadSettings";
import { DataSourceTab } from "@/components/settings/DataSourceTab";
import { TickersDbTab } from "@/components/settings/TickersDbTab";
import { TradesDbTab } from "@/components/settings/TradesDbTab";
import { DicteeTab } from "@/components/settings/DicteeTab";
import { SyncTab } from "@/components/settings/SyncTab";
import { StartupTab } from "@/components/settings/StartupTab";
import { FeedDiagnosticsTab } from "@/components/settings/FeedDiagnosticsTab";
import { NewsDebugTab } from "@/components/settings/NewsDebugTab";
import { LogsTab } from "@/components/settings/LogsTab";
import { UpdateTab } from "@/components/settings/UpdateTab";
import { useChartThemeStore, type ChartTheme, type ChartThemeSection } from "@/stores/chartThemeStore";
import { useChartInput, SENS_MIN, SENS_MAX } from "@/stores/chartInputStore";
import { api } from "@/lib/tauri";
import { cn } from "@/lib/utils";
import type { AppConfig, AttentionMode, SecretKey, SecretsUpdate, Session, StrategyRiskConfig } from "@/types";

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

/** Editable API-key fields shown in Settings → API Keys (order = display order). The
 *  tradetally_* keys are rendered in a dedicated "TradeTally" subsection below the rest. */
const SECRET_FIELDS: { key: SecretKey; label: string; type?: string }[] = [
  { key: "alpaca_key",          label: "Alpaca key" },
  { key: "alpaca_secret",       label: "Alpaca secret" },
  { key: "massive_api_key",     label: "Massive API key (float)" },
  { key: "sec_api_key",         label: "sec-api.io key (pays · industrie)" },
  { key: "fmp_api_key",         label: "FMP API key (fallback)" },
  { key: "claude_api_key",      label: "Claude API key" },
  { key: "deepseek_api_key",    label: "Deepseek API key (news / dilution)" },
];

const TRADETALLY_SECRET_FIELDS: { key: SecretKey; label: string; type?: string }[] = [
  { key: "tradetally_token",    label: "TradeTally token (tt_live_…)" },
  { key: "tradetally_email",    label: "TradeTally email (screenshots)", type: "email" },
  { key: "tradetally_password", label: "TradeTally password (screenshots)" },
];

/** One editable secret: configured indicator + a masked input. Existing values are
 *  never sent to the UI, so the input starts empty and a blank submit keeps the
 *  stored key (the placeholder makes that explicit). */
function SecretField({
  label,
  configured,
  value,
  onChange,
  type = "password",
}: {
  label: string;
  configured: boolean;
  value: string;
  onChange: (v: string) => void;
  type?: string;
}) {
  return (
    <div className="grid grid-cols-[minmax(0,1fr)_minmax(0,15rem)] items-center gap-3 py-1">
      <div className="flex min-w-0 items-center gap-1.5">
        {configured ? (
          <CheckCircle2 className="h-3.5 w-3.5 shrink-0 text-emerald-400" />
        ) : (
          <XCircle className="h-3.5 w-3.5 shrink-0 text-muted-foreground/50" />
        )}
        <span className="truncate text-sm" title={label}>{label}</span>
      </div>
      <Input
        type={type}
        value={value}
        autoComplete="off"
        spellCheck={false}
        placeholder={configured ? "•••••• (laisser vide pour garder)" : "non défini"}
        onChange={(e) => onChange(e.target.value)}
        className="h-7 text-xs"
      />
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

/** Inline number input that commits on blur or Enter. */
function NumInput({
  value,
  min = 0,
  step = "1",
  width = "w-16",
  disabled,
  onCommit,
}: {
  value: number;
  min?: number;
  step?: string;
  width?: string;
  disabled?: boolean;
  onCommit: (v: number) => void;
}) {
  const [text, setText] = useState(String(value));
  useEffect(() => { setText(String(value)); }, [value]);

  function commit() {
    const n = parseFloat(text);
    if (Number.isFinite(n) && n >= min && n !== value) onCommit(n);
    else setText(String(value));
  }

  return (
    <Input
      type="number"
      min={min}
      step={step}
      value={text}
      disabled={disabled}
      onChange={(e) => setText(e.target.value)}
      onBlur={commit}
      onKeyDown={(e) => {
        if (e.key === "Enter") (e.target as HTMLInputElement).blur();
      }}
      className={`h-6 text-xs tabular-nums ${width}`}
    />
  );
}

/** Expanded per-strategy risk settings panel. Commits any change immediately. */
function StrategyRiskPanel({
  risk,
  disabled,
  onCommit,
}: {
  risk: StrategyRiskConfig;
  disabled: boolean;
  onCommit: (cfg: StrategyRiskConfig) => void;
}) {
  const [draft, setDraft] = useState<StrategyRiskConfig>(risk);
  useEffect(() => { setDraft(risk); }, [risk]);

  function patch(partial: Partial<StrategyRiskConfig>): StrategyRiskConfig {
    const next = { ...draft, ...partial };
    setDraft(next);
    return next;
  }

  return (
    <div className="mt-2 space-y-1.5 border-t border-border/40 pt-2">
      {/* Row 1: Risque $ + type d'ordre */}
      <div className="flex flex-wrap items-center gap-x-4 gap-y-1.5">
        <div className="flex items-center gap-1.5">
          <span className="text-[10px] text-muted-foreground">Risque $</span>
          <NumInput
            value={draft.max_risk_dollars}
            min={0}
            step="1"
            width="w-16"
            disabled={disabled}
            onCommit={(v) => onCommit(patch({ max_risk_dollars: v }))}
          />
        </div>
        <div className="flex items-center gap-1.5">
          <span className="text-[10px] text-muted-foreground">Ordre</span>
          <select
            value={draft.default_order_type}
            disabled={disabled}
            onChange={(e) => onCommit(patch({ default_order_type: e.target.value as "market" | "limit" }))}
            className="h-6 rounded border border-border bg-background px-1.5 text-xs text-foreground outline-none disabled:opacity-50"
          >
            <option value="market">Market</option>
            <option value="limit">Limit</option>
          </select>
        </div>
      </div>
      {/* Row 2: TP auto */}
      <div className="flex items-center gap-2">
        <Switch
          checked={draft.auto_tp_enabled}
          disabled={disabled}
          onCheckedChange={(v) => onCommit(patch({ auto_tp_enabled: v }))}
        />
        <span className="text-[10px] text-muted-foreground">TP auto</span>
        {draft.auto_tp_enabled && (
          <>
            <span className="text-[10px] text-muted-foreground">à</span>
            <NumInput
              value={draft.auto_tp_r}
              min={0.1}
              step="0.1"
              width="w-14"
              disabled={disabled}
              onCommit={(v) => onCommit(patch({ auto_tp_r: v }))}
            />
            <span className="text-[10px] text-muted-foreground">R</span>
          </>
        )}
      </div>
      {/* Row 3: BE auto */}
      <div className="flex items-center gap-2">
        <Switch
          checked={draft.auto_be_enabled}
          disabled={disabled}
          onCheckedChange={(v) => onCommit(patch({ auto_be_enabled: v }))}
        />
        <span className="text-[10px] text-muted-foreground">BE auto</span>
        {draft.auto_be_enabled && (
          <>
            <span className="text-[10px] text-muted-foreground">à</span>
            <NumInput
              value={draft.auto_be_r}
              min={0.1}
              step="0.1"
              width="w-14"
              disabled={disabled}
              onCommit={(v) => onCommit(patch({ auto_be_r: v }))}
            />
            <span className="text-[10px] text-muted-foreground">R</span>
          </>
        )}
      </div>
    </div>
  );
}

/** TradeTally API base URL — commits on blur/Enter via its own immediate mutate
 *  (consistent with the rest of the merged API Keys tab, which doesn't go through
 *  the parent `draft` + global Save button). */
function TradeTallyUrlField() {
  const { data: config } = useLocalConfig();
  const update = useUpdateLocalConfig();
  const [text, setText] = useState("");

  useEffect(() => {
    if (config) setText(config.tradetally.api_base_url);
  }, [config?.tradetally.api_base_url]);

  function commit() {
    if (!config || text === config.tradetally.api_base_url) return;
    update.mutate({ ...config, tradetally: { api_base_url: text } });
  }

  return (
    <div className="grid grid-cols-2 items-center gap-4">
      <Label className="text-right text-xs text-muted-foreground">API base URL</Label>
      <Input
        value={text}
        onChange={(e) => setText(e.target.value)}
        onBlur={commit}
        onKeyDown={(e) => {
          if (e.key === "Enter") (e.target as HTMLInputElement).blur();
        }}
        className="h-7 text-xs"
      />
    </div>
  );
}

const HOTKEY_GROUP_LABELS: Record<HotkeyGroup, string> = {
  Toolbar:    "Barre d'outils",
  Ordres:     "Ordres",
  Analyse:    "Analyse IA",
  Timeframes: "Timeframes (pane de gauche)",
  Replay:     "Market Replay",
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

/** One labelled swatch: a native colour picker + its hex, editable. */
function ColorRow({
  label,
  value,
  onChange,
}: {
  label: string;
  value: string;
  onChange: (v: string) => void;
}) {
  return (
    <div className="flex items-center justify-between gap-3 py-1">
      <span className="truncate text-xs">{label}</span>
      <div className="flex shrink-0 items-center gap-2">
        <span className="font-mono text-[10px] uppercase text-muted-foreground tabular-nums">{value}</span>
        <input
          type="color"
          value={value}
          onChange={(e) => onChange(e.target.value)}
          className="h-6 w-9 cursor-pointer rounded border border-border bg-transparent p-0.5"
        />
      </div>
    </div>
  );
}

/** One labelled opacity slider (0–100 %). */
function OpacityRow({
  label,
  value,
  onChange,
}: {
  label: string;
  value: number;
  onChange: (v: number) => void;
}) {
  return (
    <div className="flex items-center justify-between gap-3 py-1">
      <span className="truncate text-xs">{label}</span>
      <div className="flex shrink-0 items-center gap-2">
        <span className="w-9 text-right font-mono text-[10px] text-muted-foreground tabular-nums">
          {Math.round(value * 100)}%
        </span>
        <input
          type="range"
          min={0}
          max={1}
          step={0.01}
          value={value}
          onChange={(e) => onChange(parseFloat(e.target.value))}
          className="h-1.5 w-28 cursor-pointer accent-blue-500"
        />
      </div>
    </div>
  );
}

/** A titled group of appearance rows. */
function ThemeGroup({ title, children }: { title: string; children: ReactNode }) {
  return (
    <div>
      <div className="mb-0.5 text-[10px] font-semibold uppercase tracking-wide text-muted-foreground/60">
        {title}
      </div>
      <div className="rounded-md border border-border px-3 py-1.5">{children}</div>
    </div>
  );
}

/** Settings → Apparence: live chart-palette editor backed by `chartThemeStore`.
 *  Edits apply immediately to every open pane; values persist locally. The
 *  shipped defaults live in `DEFAULT_CHART_THEME` (see the store header). */
function AppearanceTab() {
  const theme = useChartThemeStore((s) => s.theme);
  const setField = useChartThemeStore((s) => s.set);
  const reset = useChartThemeStore((s) => s.reset);
  const [copied, setCopied] = useState(false);

  // Wheel / trackpad tuning (frontend pref, persisted in chartInputStore).
  const mouseSensitivity  = useChartInput((s) => s.mouseSensitivity);
  const zoomSensitivity   = useChartInput((s) => s.zoomSensitivity);
  const zoomInvert        = useChartInput((s) => s.zoomInvert);
  const scrollSwapAxes    = useChartInput((s) => s.scrollSwapAxes);
  const setMouseSensitivity = useChartInput((s) => s.setMouseSensitivity);
  const setZoomSensitivity  = useChartInput((s) => s.setZoomSensitivity);
  const setZoomInvert       = useChartInput((s) => s.setZoomInvert);
  const setScrollSwapAxes   = useChartInput((s) => s.setScrollSwapAxes);

  // Typed thin wrappers so each row stays a one-liner.
  function color<S extends ChartThemeSection>(section: S, key: keyof ChartTheme[S]) {
    return {
      value: theme[section][key] as unknown as string,
      onChange: (v: string) => setField(section, key, v as ChartTheme[S][keyof ChartTheme[S]]),
    };
  }
  function opacity<S extends ChartThemeSection>(section: S, key: keyof ChartTheme[S]) {
    return {
      value: theme[section][key] as unknown as number,
      onChange: (v: number) => setField(section, key, v as ChartTheme[S][keyof ChartTheme[S]]),
    };
  }

  function copyJson() {
    void navigator.clipboard.writeText(JSON.stringify(theme, null, 2)).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    });
  }

  return (
    <div className="space-y-3">
      <p className="text-xs text-muted-foreground">
        Couleurs et opacités des graphiques. Les changements s'appliquent
        immédiatement à tous les panes ouverts et sont conservés au relancement.
      </p>
      <div className="space-y-3">
        <ThemeGroup title="Navigation (molette / trackpad)">
          <div className="flex items-center justify-between gap-3 py-0.5">
            <span className="text-xs text-muted-foreground">
              Molette souris — sensibilité
            </span>
            <div className="flex items-center gap-2">
              <input
                type="range"
                min={SENS_MIN}
                max={SENS_MAX}
                step={0.1}
                value={mouseSensitivity}
                onChange={(e) => setMouseSensitivity(parseFloat(e.target.value))}
                className="w-32 accent-blue-500"
              />
              <span className="w-9 text-right text-[10px] tabular-nums text-muted-foreground">
                {mouseSensitivity.toFixed(1)}×
              </span>
            </div>
          </div>
          <div className="flex items-center justify-between gap-3 py-0.5">
            <span className="text-xs text-muted-foreground">
              Scroll 2 doigts — sensibilité
            </span>
            <div className="flex items-center gap-2">
              <input
                type="range"
                min={SENS_MIN}
                max={SENS_MAX}
                step={0.1}
                value={zoomSensitivity}
                onChange={(e) => setZoomSensitivity(parseFloat(e.target.value))}
                className="w-32 accent-blue-500"
              />
              <span className="w-9 text-right text-[10px] tabular-nums text-muted-foreground">
                {zoomSensitivity.toFixed(1)}×
              </span>
            </div>
          </div>
          <div className="flex items-center justify-between gap-3 py-0.5">
            <span className="text-xs text-muted-foreground">
              Scroll 2 doigts — inverser le sens du zoom
            </span>
            <input
              type="checkbox"
              checked={zoomInvert}
              onChange={(e) => setZoomInvert(e.target.checked)}
              className="h-4 w-4 accent-blue-500"
            />
          </div>
          <div className="flex items-center justify-between gap-3 py-0.5">
            <span className="text-xs text-muted-foreground">
              Scroll 2 doigts — inverser vertical/horizontal
              <span className="ml-1 text-muted-foreground/60">
                (défaut : vertical→zoom X, horizontal→zoom Y)
              </span>
            </span>
            <input
              type="checkbox"
              checked={scrollSwapAxes}
              onChange={(e) => setScrollSwapAxes(e.target.checked)}
              className="h-4 w-4 accent-blue-500"
            />
          </div>
        </ThemeGroup>

        <ThemeGroup title="Bougies">
          <ColorRow label="Hausse" {...color("candle", "up")} />
          <ColorRow label="Baisse" {...color("candle", "down")} />
        </ThemeGroup>

        <ThemeGroup title="Volume (couleur des bougies)">
          <OpacityRow label="Opacité hausse" {...opacity("volume", "upOpacity")} />
          <OpacityRow label="Opacité baisse" {...opacity("volume", "downOpacity")} />
        </ThemeGroup>

        <ThemeGroup title="Fond pré / post-market">
          <ColorRow label="Teinte" {...color("session", "color")} />
          <OpacityRow label="Opacité" {...opacity("session", "opacity")} />
        </ThemeGroup>

        <ThemeGroup title="Grille">
          <ColorRow label="Couleur" {...color("grid", "color")} />
          <OpacityRow label="Opacité" {...opacity("grid", "opacity")} />
        </ThemeGroup>

        <ThemeGroup title="Indicateurs">
          <ColorRow label="VWAP" {...color("indicators", "vwap")} />
          <ColorRow label="EMA" {...color("indicators", "ema")} />
          <ColorRow label="SMA" {...color("indicators", "sma")} />
          <ColorRow label="Bollinger" {...color("indicators", "bollinger")} />
          <OpacityRow label="Bollinger (opacité)" {...opacity("indicators", "bollingerOpacity")} />
        </ThemeGroup>

        <ThemeGroup title="Exécutions (marqueurs)">
          <ColorRow label="Achat ▶" {...color("executions", "buy")} />
          <ColorRow label="Vente ◀" {...color("executions", "sell")} />
          <ColorRow label="Ligne gain" {...color("executions", "profit")} />
          <ColorRow label="Ligne perte" {...color("executions", "loss")} />
        </ThemeGroup>

        <ThemeGroup title="Marqueurs">
          <ColorRow label="Splits" {...color("markers", "split")} />
          <ColorRow label="News (pastille)" {...color("markers", "news")} />
        </ThemeGroup>

        <ThemeGroup title="Niveaux">
          <ColorRow label="Stop Loss" {...color("levels", "sl")} />
          <ColorRow label="Take Profit" {...color("levels", "tp")} />
          <ColorRow label="Alarme" {...color("levels", "alarm")} />
          <ColorRow label="Ordre Limite" {...color("levels", "limit")} />
        </ThemeGroup>
      </div>

      <div className="flex items-center justify-between gap-2">
        <Button variant="ghost" size="sm" onClick={reset}>
          Réinitialiser
        </Button>
        <Button variant="outline" size="sm" onClick={copyJson}>
          {copied ? "Copié ✓" : "Copier le thème (JSON)"}
        </Button>
      </div>
    </div>
  );
}

// ─── Sidebar: tabs grouped by category ──────────────────────────────────────

type TabId =
  | "trading" | "strategies"
  | "apparence" | "keyboard" | "gamepad" | "notifs"
  | "data-source" | "latency" | "feed-diagnostics" | "news-debug"
  | "tickers-db" | "trades-db" | "dictee" | "tags"
  | "secrets" | "sync" | "startup" | "logs" | "update";

interface TabDef { id: TabId; label: string; icon: LucideIcon }
interface TabGroup { label: string; tabs: TabDef[] }

const TAB_GROUPS: TabGroup[] = [
  {
    label: "Trading",
    tabs: [
      { id: "trading",    label: "Trading",    icon: LineChart },
      { id: "strategies", label: "Stratégies", icon: ListChecks },
    ],
  },
  {
    label: "Interface",
    tabs: [
      { id: "apparence", label: "Apparence",     icon: Palette },
      { id: "keyboard",  label: "Clavier",       icon: Keyboard },
      { id: "gamepad",   label: "Manette Xbox",  icon: Gamepad2 },
      { id: "notifs",    label: "Notifications", icon: Bell },
    ],
  },
  {
    label: "Flux de données",
    tabs: [
      { id: "data-source",       label: "Source de données", icon: Database },
      { id: "latency",           label: "Latency",           icon: Gauge },
      { id: "feed-diagnostics",  label: "Flux live",         icon: Radio },
      { id: "news-debug",        label: "News premarket",    icon: Newspaper },
    ],
  },
  {
    label: "Données stockées",
    tabs: [
      { id: "tickers-db", label: "Tickers (DB)",  icon: Table2 },
      { id: "trades-db",  label: "Trades (DB)",   icon: Receipt },
      { id: "dictee",     label: "Dictée vocale", icon: Mic },
      { id: "tags",       label: "Tags",          icon: Tag },
    ],
  },
  {
    label: "Comptes & Système",
    tabs: [
      { id: "secrets", label: "API Keys",              icon: KeyRound },
      { id: "sync",    label: "Sync TradeTally",       icon: RefreshCw },
      { id: "startup", label: "Pipeline de démarrage", icon: Play },
      { id: "logs",    label: "Logs",                  icon: ScrollText },
      { id: "update",  label: "Mise à jour",           icon: DownloadCloud },
    ],
  },
];

export function SettingsModal({ open, onClose }: Props) {
  const { data: config } = useLocalConfig();
  const { data: secrets } = useSecretsStatus();
  const { data: strategies = [] } = useStrategies();
  const setStrategyEnabled = useSetStrategyEnabled();
  const setStrategyRisk = useSetStrategyRisk();
  const update = useUpdateLocalConfig();
  const updateSecrets = useUpdateSecrets();

  const [draft, setDraft] = useState<AppConfig | null>(null);
  const [tagInput, setTagInput] = useState("");
  // Secret inputs are local + write-only: we only ever send the keys the user
  // typed (non-empty), and clear them after a successful save.
  const [secretInputs, setSecretInputs] = useState<SecretsUpdate>({});
  const hasSecretEdits = Object.values(secretInputs).some((v) => v.trim() !== "");

  function saveSecrets() {
    const updates: SecretsUpdate = {};
    for (const { key } of [...SECRET_FIELDS, ...TRADETALLY_SECRET_FIELDS]) {
      const v = secretInputs[key]?.trim();
      if (v) updates[key] = v;
    }
    if (Object.keys(updates).length === 0) return;
    updateSecrets.mutate(updates, { onSuccess: () => setSecretInputs({}) });
  }

  useEffect(() => {
    if (config) setDraft(structuredClone(config));
  }, [config]);

  // Reset transient state when opening the modal (the component is never
  // unmounted, so useState initialisers only run once).
  useEffect(() => {
    if (open) {
      setSecretInputs({});
    }
  }, [open]);

  const tags = draft?.journal?.tags ?? [];
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
    update.mutate(draft, {
      onSuccess: () => {
        // Build/destroy the flash overlay on demand from here — it's never created
        // at startup, only when the user has the flash cue enabled.
        api.setFlashOverlay(draft.ui.flash_alerts !== "off").catch(() => {});
        onClose();
      },
    });
  }

  return (
    <Dialog open={open} onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="flex h-[85vh] max-h-[760px] w-[1040px] max-w-[94vw] flex-col gap-0 overflow-hidden p-0">
        <DialogHeader className="shrink-0 border-b border-border px-4 py-3">
          <DialogTitle>Settings</DialogTitle>
        </DialogHeader>

        {!draft ? (
          <div className="flex flex-1 items-center justify-center text-sm text-muted-foreground">
            Chargement…
          </div>
        ) : (
          <Tabs defaultValue="trading" orientation="vertical" className="flex min-h-0 flex-1 flex-row">
            {/* Sidebar — always visible, grouped by category. */}
            <TabsList className="flex h-full w-56 shrink-0 flex-col items-stretch justify-start gap-0.5 overflow-y-auto rounded-none border-r border-border bg-card/40 p-2">
              {TAB_GROUPS.map((group) => (
                <div key={group.label} className="mt-3 first:mt-0">
                  <div className="mb-1 px-2 text-[10px] font-semibold uppercase tracking-wide text-muted-foreground/60">
                    {group.label}
                  </div>
                  {group.tabs.map((t) => (
                    <TabsTrigger
                      key={t.id}
                      value={t.id}
                      className="w-full justify-start gap-2 rounded-md px-2.5 py-1.5 text-left text-xs font-normal text-muted-foreground data-[state=active]:bg-accent data-[state=active]:text-foreground data-[state=active]:shadow-none"
                    >
                      <t.icon className="h-3.5 w-3.5 shrink-0" />
                      <span className="truncate">{t.label}</span>
                    </TabsTrigger>
                  ))}
                </div>
              ))}
            </TabsList>

            {/* Content pane — scrolls independently of the sidebar. */}
            <div className="min-w-0 flex-1 overflow-y-auto p-4">
              {/* ── Trading ── */}
              <TabsContent value="trading" className="mt-0 space-y-3">
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

              {/* ── Stratégies (on/off + risk complet par stratégie) ── */}
              <TabsContent value="strategies" className="mt-0 space-y-2">
                <p className="text-xs text-muted-foreground">
                  Active/désactive et configure le risque de chaque stratégie. Effet
                  immédiat, conservé au relancement.
                </p>
                <div className="space-y-2">
                  {strategies.length === 0 && (
                    <span className="text-xs text-muted-foreground/60">Aucune stratégie.</span>
                  )}
                  {strategies.map((s) => (
                    <div
                      key={s.id}
                      className="rounded-md border border-border px-3 py-2"
                    >
                      {/* Header row: name + sessions + toggle */}
                      <div className="flex items-center justify-between gap-3">
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
                        <Switch
                          checked={s.enabled}
                          disabled={setStrategyEnabled.isPending}
                          onCheckedChange={(v) =>
                            setStrategyEnabled.mutate({ id: s.id, enabled: v })
                          }
                        />
                      </div>
                      {/* Risk settings panel */}
                      <StrategyRiskPanel
                        risk={s.risk}
                        disabled={setStrategyRisk.isPending}
                        onCommit={(risk) => setStrategyRisk.mutate({ id: s.id, risk })}
                      />
                    </div>
                  ))}
                </div>
              </TabsContent>

              {/* ── Apparence (chart palette: colours + opacities, live) ── */}
              <TabsContent value="apparence" className="mt-0">
                <AppearanceTab />
              </TabsContent>

              {/* ── Clavier / Souris — chord recorder ── */}
              <TabsContent value="keyboard" className="mt-0 space-y-3">
                <p className="text-xs text-muted-foreground">
                  Assigne une touche, une combinaison clavier ou un bouton de souris
                  (boutons latéraux d'une souris multi-boutons) à chaque commande.
                  Clique sur le champ puis appuie sur la touche/bouton voulu&nbsp;;
                  <kbd className="mx-1 rounded bg-muted px-1 text-[10px]">Échap</kbd>
                  annule. Le raccourci agit sur la zone <strong>survolée par la
                  souris</strong> (son pane de gauche), sinon sur la zone active. Clic
                  gauche/droit réservés.
                </p>
                <div className="space-y-3">
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

              {/* ── Manette Xbox — bindings, sensibilité, test en direct ── */}
              <TabsContent value="gamepad" className="mt-0">
                <GamepadSettings />
              </TabsContent>

              {/* ── Notifications (native OS desktop alerts) ── */}
              <TabsContent value="notifs" className="mt-0 space-y-3">
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
                      même si TagDash est caché derrière d'autres fenêtres. L'overlay
                      est créé à l'enregistrement (jamais au démarrage) ; choisis
                      «&nbsp;Désactivé&nbsp;» pour le fermer.
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

                {/* Sound cue: when (by session) + which sound, each previewable. */}
                <div className="rounded-md border border-border px-3 py-2.5">
                  <div className="flex items-center justify-between gap-4">
                    <div className="min-w-0">
                      <div className="text-sm font-medium">Son de notification</div>
                      <p className="mt-0.5 text-xs text-muted-foreground">
                        Joue un son léger et discret à chaque alerte du scanner. Choisis
                        le moment de la session et le son ci-dessous (▶ pour l'écouter).
                      </p>
                    </div>
                    <AttentionSelect
                      value={draft.ui.alert_sound_mode}
                      onChange={(v) => set("ui", "alert_sound_mode", v)}
                    />
                  </div>
                  <div className="mt-2 space-y-1">
                    {NOTIF_SOUNDS.map((s) => {
                      const selected = draft.ui.alert_sound === s.id;
                      return (
                        <div
                          key={s.id}
                          className={cn(
                            "flex items-center justify-between gap-2 rounded-md border px-2.5 py-1.5",
                            selected ? "border-blue-500/70 bg-blue-900/20" : "border-border/60",
                          )}
                        >
                          <button
                            onClick={() => set("ui", "alert_sound", s.id)}
                            className="flex min-w-0 flex-1 items-center gap-2 text-left"
                          >
                            <span
                              className={cn(
                                "h-2.5 w-2.5 shrink-0 rounded-full border",
                                selected ? "border-blue-400 bg-blue-400" : "border-muted-foreground/40",
                              )}
                            />
                            <span className="truncate text-xs">{s.label}</span>
                          </button>
                          <button
                            onClick={() => playNotifSound(s.id)}
                            title="Écouter"
                            className="flex h-6 w-6 shrink-0 items-center justify-center rounded text-muted-foreground hover:bg-accent hover:text-foreground"
                          >
                            <Play className="h-3 w-3" />
                          </button>
                        </div>
                      );
                    })}
                  </div>
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

              {/* ── Source de données (flat files + toggle API/flat files) ── */}
              <TabsContent value="data-source" className="mt-0">
                <DataSourceTab />
              </TabsContent>

              {/* ── Latency ── */}
              <TabsContent value="latency" className="mt-0 space-y-3">
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

              {/* ── Flux live (Alpaca feed diagnostics) ── */}
              <TabsContent value="feed-diagnostics" className="mt-0">
                <FeedDiagnosticsTab />
              </TabsContent>

              {/* ── News premarket diagnostics ── */}
              <TabsContent value="news-debug" className="mt-0">
                <NewsDebugTab />
              </TabsContent>

              {/* ── Tickers (DB) ── */}
              <TabsContent value="tickers-db" className="mt-0 h-full">
                <TickersDbTab />
              </TabsContent>

              {/* ── Trades (DB) ── */}
              <TabsContent value="trades-db" className="mt-0 h-full">
                <TradesDbTab />
              </TabsContent>

              {/* ── Dictée vocale (config + mic test + queue) ── */}
              <TabsContent value="dictee" className="mt-0">
                <DicteeTab />
              </TabsContent>

              {/* ── Tags (user-defined journal tags) ── */}
              <TabsContent value="tags" className="mt-0 space-y-3">
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

              {/* ── API Keys (status only) + TradeTally (URL + token/email/password) ── */}
              <TabsContent value="secrets" className="mt-0 space-y-1">
                <p className="mb-3 text-xs text-muted-foreground">
                  Saisis tes clés ci-dessous puis <strong>Enregistrer les clés</strong> —
                  elles sont écrites dans{" "}
                  <code className="rounded bg-muted px-1 py-0.5 text-[11px]">
                    tagdash.secrets.toml
                  </code>
                  . Un champ laissé vide conserve la clé existante. Les valeurs ne sont
                  jamais relues par l'interface (seul l'état configuré/non est affiché).
                </p>
                <div className="space-y-0.5">
                  {SECRET_FIELDS.map((f) => (
                    <SecretField
                      key={f.key}
                      label={f.label}
                      type={f.type}
                      configured={secrets?.[f.key] ?? false}
                      value={secretInputs[f.key] ?? ""}
                      onChange={(v) => setSecretInputs((s) => ({ ...s, [f.key]: v }))}
                    />
                  ))}
                </div>

                <Separator className="my-3" />
                <div className="mb-2 text-[10px] font-semibold uppercase tracking-wide text-muted-foreground/60">
                  TradeTally
                </div>
                <div className="space-y-2">
                  <TradeTallyUrlField />
                  {TRADETALLY_SECRET_FIELDS.map((f) => (
                    <SecretField
                      key={f.key}
                      label={f.label}
                      type={f.type}
                      configured={secrets?.[f.key] ?? false}
                      value={secretInputs[f.key] ?? ""}
                      onChange={(v) => setSecretInputs((s) => ({ ...s, [f.key]: v }))}
                    />
                  ))}
                </div>

                <div className="mt-3 flex items-center justify-end gap-2">
                  {updateSecrets.isSuccess && !hasSecretEdits && (
                    <span className="text-xs text-emerald-400">Clés enregistrées ✓</span>
                  )}
                  <Button
                    size="sm"
                    onClick={saveSecrets}
                    disabled={updateSecrets.isPending || !hasSecretEdits}
                  >
                    {updateSecrets.isPending ? "Enregistrement…" : "Enregistrer les clés"}
                  </Button>
                </div>
              </TabsContent>

              {/* ── Sync TradeTally ── */}
              <TabsContent value="sync" className="mt-0">
                <SyncTab />
              </TabsContent>

              {/* ── Pipeline de démarrage (read-only review) ── */}
              <TabsContent value="startup" className="mt-0">
                <StartupTab />
              </TabsContent>

              {/* ── Logs ── */}
              <TabsContent value="logs" className="mt-0 h-full">
                <LogsTab />
              </TabsContent>

              {/* ── Mise à jour ── */}
              <TabsContent value="update" className="mt-0">
                <UpdateTab />
              </TabsContent>
            </div>
          </Tabs>
        )}

        {draft && (
          <div className="flex shrink-0 items-center justify-end gap-2 border-t border-border px-4 py-3">
            <Button variant="ghost" size="sm" onClick={onClose}>
              Cancel
            </Button>
            <Button
              size="sm"
              onClick={save}
              disabled={update.isPending || !draft}
            >
              {update.isPending ? "Saving…" : "Save"}
            </Button>
          </div>
        )}
      </DialogContent>
    </Dialog>
  );
}
