// API secrets. Never exposed to the React frontend — only used by Rust modules.
// Stored in tagdash.secrets.toml next to tagdash.toml.
// The frontend only ever receives a SecretsStatus (booleans — is each key set?).

use serde::{Deserialize, Serialize};
use std::path::Path;

/// Actual secrets — Deserialize only, intentionally no Serialize to prevent
/// accidental serialisation and exposure to the frontend.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Secrets {
    pub alpaca_key: Option<String>,
    pub alpaca_secret: Option<String>,
    /// FMP (Financial Modeling Prep). Kept for fallback / legacy — the float
    /// provider is now Massive (see `massive_api_key`).
    pub fmp_api_key: Option<String>,
    /// Massive (api.massive.com) — bulk free-float data, the active float source.
    pub massive_api_key: Option<String>,
    /// sec-api.io — company country of origin + SIC industry classification.
    pub sec_api_key: Option<String>,
    pub claude_api_key: Option<String>,
    /// Deepseek (api.deepseek.com) — LLM used by the micro_pullback enrichment
    /// (news bluff/solid read + dilution-risk read). Wiring is present even when
    /// the key is absent (the calls are simply skipped).
    pub deepseek_api_key: Option<String>,
    pub tradetally_token: Option<String>,
    /// Session credentials — only needed to upload screenshot images, which the
    /// API token cannot do (the /images route requires a logged-in session).
    pub tradetally_email: Option<String>,
    pub tradetally_password: Option<String>,
}

/// Safe status sent to the frontend: true = key is present and non-empty.
#[derive(Debug, Clone, Serialize)]
pub struct SecretsStatus {
    pub alpaca_key: bool,
    pub alpaca_secret: bool,
    pub fmp_api_key: bool,
    pub massive_api_key: bool,
    pub sec_api_key: bool,
    pub claude_api_key: bool,
    pub deepseek_api_key: bool,
    pub tradetally_token: bool,
    pub tradetally_email: bool,
    pub tradetally_password: bool,
}

/// Partial update sent from Settings → API Keys. Only fields the user actually
/// typed are `Some`; a blank/whitespace value is ignored so an existing secret is
/// never wiped by an empty input. Deserialize-only (write path), never returned.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SecretsUpdate {
    pub alpaca_key: Option<String>,
    pub alpaca_secret: Option<String>,
    pub fmp_api_key: Option<String>,
    pub massive_api_key: Option<String>,
    pub sec_api_key: Option<String>,
    pub claude_api_key: Option<String>,
    pub deepseek_api_key: Option<String>,
    pub tradetally_token: Option<String>,
    pub tradetally_email: Option<String>,
    pub tradetally_password: Option<String>,
}

impl Secrets {
    pub fn status(&self) -> SecretsStatus {
        let set = |o: &Option<String>| o.as_deref().map(|s| !s.is_empty()).unwrap_or(false);
        SecretsStatus {
            alpaca_key: set(&self.alpaca_key),
            alpaca_secret: set(&self.alpaca_secret),
            fmp_api_key: set(&self.fmp_api_key),
            massive_api_key: set(&self.massive_api_key),
            sec_api_key: set(&self.sec_api_key),
            claude_api_key: set(&self.claude_api_key),
            deepseek_api_key: set(&self.deepseek_api_key),
            tradetally_token: set(&self.tradetally_token),
            tradetally_email: set(&self.tradetally_email),
            tradetally_password: set(&self.tradetally_password),
        }
    }

    /// Apply a partial update in place. A `Some` non-empty (trimmed) value replaces
    /// the field; `None` or a blank value leaves the current secret untouched.
    pub fn apply_update(&mut self, u: SecretsUpdate) {
        fn set(dst: &mut Option<String>, v: Option<String>) {
            if let Some(s) = v {
                let t = s.trim();
                if !t.is_empty() {
                    *dst = Some(t.to_string());
                }
            }
        }
        set(&mut self.alpaca_key, u.alpaca_key);
        set(&mut self.alpaca_secret, u.alpaca_secret);
        set(&mut self.fmp_api_key, u.fmp_api_key);
        set(&mut self.massive_api_key, u.massive_api_key);
        set(&mut self.sec_api_key, u.sec_api_key);
        set(&mut self.claude_api_key, u.claude_api_key);
        set(&mut self.deepseek_api_key, u.deepseek_api_key);
        set(&mut self.tradetally_token, u.tradetally_token);
        set(&mut self.tradetally_email, u.tradetally_email);
        set(&mut self.tradetally_password, u.tradetally_password);
    }
}

/// Persist secrets to `<app_dir>/tagdash.secrets.toml`. Written by hand (Secrets is
/// deliberately NOT `Serialize`, so values can never be sent to the frontend by an
/// accidental derive). Called when the user edits keys in Settings → API Keys.
pub fn save(app_dir: &Path, s: &Secrets) -> Result<(), String> {
    fn esc(o: &Option<String>) -> String {
        o.as_deref().unwrap_or("").replace('\\', "\\\\").replace('"', "\\\"")
    }
    let path = app_dir.join("tagdash.secrets.toml");
    let content = format!(
        "# TagDash — API secrets (managed from Settings → API Keys).\n\
         # NEVER sent to the frontend. Keep it out of version control.\n\n\
         alpaca_key          = \"{}\"\n\
         alpaca_secret       = \"{}\"\n\
         fmp_api_key         = \"{}\"\n\
         massive_api_key     = \"{}\"\n\
         sec_api_key         = \"{}\"\n\
         claude_api_key      = \"{}\"\n\
         deepseek_api_key    = \"{}\"\n\
         tradetally_token    = \"{}\"\n\
         tradetally_email    = \"{}\"\n\
         tradetally_password = \"{}\"\n",
        esc(&s.alpaca_key),
        esc(&s.alpaca_secret),
        esc(&s.fmp_api_key),
        esc(&s.massive_api_key),
        esc(&s.sec_api_key),
        esc(&s.claude_api_key),
        esc(&s.deepseek_api_key),
        esc(&s.tradetally_token),
        esc(&s.tradetally_email),
        esc(&s.tradetally_password),
    );
    std::fs::write(&path, content).map_err(|e| e.to_string())
}

/// Load secrets from `<app_dir>/tagdash.secrets.toml`.
/// If the file is absent or unreadable, return empty defaults.
/// Creates a template file on first run.
pub fn load(app_dir: &Path) -> Secrets {
    let path = app_dir.join("tagdash.secrets.toml");

    if !path.exists() {
        let template = r#"# TagDash — API secrets
# Edit this file to configure your API keys.
# This file is NEVER sent to the frontend.
# Keep it out of version control.

alpaca_key       = ""
alpaca_secret    = ""
# FMP is kept for fallback/legacy; Massive is the active float provider.
fmp_api_key      = ""
# Massive (api.massive.com) — bulk free-float data.
massive_api_key  = ""
# sec-api.io — company country of origin + SIC industry.
sec_api_key      = ""
claude_api_key   = ""
# Deepseek (api.deepseek.com) — micro_pullback news/dilution analysis.
deepseek_api_key = ""
# TradeTally API key. Since the v1 auth update this MUST be a scoped API key,
# NOT an old personal token: the server only treats the Bearer token as an API
# key when it starts with "tt_live_" (or "tt_test_") — anything else is parsed
# as a JWT and rejected (401). Create it in TradeTally → Settings → API Keys
# (Pro tier required) with the scopes "trades:read" and "trades:write".
tradetally_token = ""

# Optional — only used to upload chart screenshots to TradeTally.
# The API token above cannot upload images (the /images endpoint needs a login
# session), so set your TradeTally login here to enable screenshot upload.
tradetally_email    = ""
tradetally_password = ""
"#;
        let _ = std::fs::write(&path, template);
        return Secrets::default();
    }

    match std::fs::read_to_string(&path) {
        Ok(content) => toml::from_str::<Secrets>(&content).unwrap_or_default(),
        Err(_) => Secrets::default(),
    }
}
