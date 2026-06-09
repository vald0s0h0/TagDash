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
import type { AppConfig, Session } from "@/types";

const SESSION_LABELS: Record<Session, string> = {
  premarket:  "Premarket",
  pre_open:   "Pre-open",
  open:       "Open",
  afterhours: "Afterhours",
};

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
