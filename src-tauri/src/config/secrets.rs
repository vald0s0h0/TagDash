// API secrets. Never exposed to the React frontend — only used by Rust modules.
// Stored in <root>/user/secrets.toml, next to settings.toml (see `crate::paths`).
// The frontend only ever receives a SecretsStatus (booleans — is each key set?).

use serde::{Deserialize, Serialize};
use std::path::Path;

/// Actual secrets — Deserialize only, intentionally no Serialize to prevent
/// accidental serialisation and exposure to the frontend.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Secrets {
    /// DERIVED market-data credentials — not typed by the user.
    ///
    /// Alpaca issues exactly one key pair per account, and that pair authenticates
    /// both the Trading API and the Market Data API; there is no such thing as a
    /// standalone "market data key". So this pair is a copy of whichever account
    /// pair is currently selected, refreshed by [`Secrets::resolve_market_data`].
    /// It exists because ~30 call sites read `alpaca_key` to fetch bars, and they
    /// have no business knowing which account they are borrowing from.
    pub alpaca_key: Option<String>,
    pub alpaca_secret: Option<String>,
    /// The paper account's key pair (`paper-api.alpaca.markets` + market data).
    pub alpaca_paper_key: Option<String>,
    pub alpaca_paper_secret: Option<String>,
    /// The REAL-MONEY account's key pair (`api.alpaca.markets` + market data).
    /// Kept in their own fields so selecting "live" cannot silently reuse a paper
    /// key — or, far worse, the reverse.
    pub alpaca_live_key: Option<String>,
    pub alpaca_live_secret: Option<String>,
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
    /// Tavily (api.tavily.com) — open-web search, used ONLY as the last resort of
    /// the Panic Score context cascade (Alpaca news → Massive news → web) when no
    /// structured source explains a move. Preferred over Brave when both are set:
    /// it returns extracted article text plus a synthesised answer, so there is
    /// less snippet noise to hand the model. See `crate::websearch`.
    pub tavily_api_key: Option<String>,
    /// Brave Search (api.search.brave.com) — the alternative web-search provider
    /// for the same cascade step. Used when no Tavily key is present.
    pub brave_api_key: Option<String>,
    pub tradetally_token: Option<String>,
    /// Session credentials — only needed to upload screenshot images, which the
    /// API token cannot do (the /images route requires a logged-in session).
    pub tradetally_email: Option<String>,
    pub tradetally_password: Option<String>,
    /// ntfy server base URL. Optional: unset means the public https://ntfy.sh.
    /// Kept here rather than in `settings.toml` because it travels with the topic,
    /// and a self-hosted URL is often as identifying as the topic itself.
    pub ntfy_server: Option<String>,
    /// ntfy topic to publish to. THIS IS A SECRET on the public server: anyone who
    /// knows a topic name can read it and publish to it — there is no other access
    /// control. Treat it exactly like a key.
    pub ntfy_topic: Option<String>,
    /// ntfy access token (`tk_…`). Only needed for a protected/self-hosted topic;
    /// a public ntfy.sh topic publishes without any auth.
    pub ntfy_token: Option<String>,
}

/// Safe status sent to the frontend: true = key is present and non-empty.
#[derive(Debug, Clone, Serialize)]
pub struct SecretsStatus {
    pub alpaca_key: bool,
    pub alpaca_secret: bool,
    pub alpaca_paper_key: bool,
    pub alpaca_paper_secret: bool,
    pub alpaca_live_key: bool,
    pub alpaca_live_secret: bool,
    pub fmp_api_key: bool,
    pub massive_api_key: bool,
    pub sec_api_key: bool,
    pub claude_api_key: bool,
    pub deepseek_api_key: bool,
    pub tavily_api_key: bool,
    pub brave_api_key: bool,
    pub tradetally_token: bool,
    pub tradetally_email: bool,
    pub tradetally_password: bool,
    pub ntfy_server: bool,
    pub ntfy_topic: bool,
    pub ntfy_token: bool,
}

/// Partial update sent from Settings → API Keys. Only fields the user actually
/// typed are `Some`; a blank/whitespace value is ignored so an existing secret is
/// never wiped by an empty input. Deserialize-only (write path), never returned.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SecretsUpdate {
    pub alpaca_key: Option<String>,
    pub alpaca_secret: Option<String>,
    pub alpaca_paper_key: Option<String>,
    pub alpaca_paper_secret: Option<String>,
    pub alpaca_live_key: Option<String>,
    pub alpaca_live_secret: Option<String>,
    pub fmp_api_key: Option<String>,
    pub massive_api_key: Option<String>,
    pub sec_api_key: Option<String>,
    pub claude_api_key: Option<String>,
    pub deepseek_api_key: Option<String>,
    pub tavily_api_key: Option<String>,
    pub brave_api_key: Option<String>,
    pub tradetally_token: Option<String>,
    pub tradetally_email: Option<String>,
    pub tradetally_password: Option<String>,
    pub ntfy_server: Option<String>,
    pub ntfy_topic: Option<String>,
    pub ntfy_token: Option<String>,
}

impl Secrets {
    pub fn status(&self) -> SecretsStatus {
        let set = |o: &Option<String>| o.as_deref().map(|s| !s.is_empty()).unwrap_or(false);
        SecretsStatus {
            alpaca_key: set(&self.alpaca_key),
            alpaca_secret: set(&self.alpaca_secret),
            alpaca_paper_key: set(&self.alpaca_paper_key),
            alpaca_paper_secret: set(&self.alpaca_paper_secret),
            alpaca_live_key: set(&self.alpaca_live_key),
            alpaca_live_secret: set(&self.alpaca_live_secret),
            fmp_api_key: set(&self.fmp_api_key),
            massive_api_key: set(&self.massive_api_key),
            sec_api_key: set(&self.sec_api_key),
            claude_api_key: set(&self.claude_api_key),
            deepseek_api_key: set(&self.deepseek_api_key),
            tavily_api_key: set(&self.tavily_api_key),
            brave_api_key: set(&self.brave_api_key),
            tradetally_token: set(&self.tradetally_token),
            tradetally_email: set(&self.tradetally_email),
            tradetally_password: set(&self.tradetally_password),
            ntfy_server: set(&self.ntfy_server),
            ntfy_topic: set(&self.ntfy_topic),
            ntfy_token: set(&self.ntfy_token),
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
        set(&mut self.alpaca_paper_key, u.alpaca_paper_key);
        set(&mut self.alpaca_paper_secret, u.alpaca_paper_secret);
        set(&mut self.alpaca_live_key, u.alpaca_live_key);
        set(&mut self.alpaca_live_secret, u.alpaca_live_secret);
        set(&mut self.fmp_api_key, u.fmp_api_key);
        set(&mut self.massive_api_key, u.massive_api_key);
        set(&mut self.sec_api_key, u.sec_api_key);
        set(&mut self.claude_api_key, u.claude_api_key);
        set(&mut self.deepseek_api_key, u.deepseek_api_key);
        set(&mut self.tavily_api_key, u.tavily_api_key);
        set(&mut self.brave_api_key, u.brave_api_key);
        set(&mut self.tradetally_token, u.tradetally_token);
        set(&mut self.tradetally_email, u.tradetally_email);
        set(&mut self.tradetally_password, u.tradetally_password);
        set(&mut self.ntfy_server, u.ntfy_server);
        set(&mut self.ntfy_topic, u.ntfy_topic);
        set(&mut self.ntfy_token, u.ntfy_token);
    }

    /// Is this pair usable (both halves present and non-blank)?
    fn pair(k: &Option<String>, s: &Option<String>) -> Option<(String, String)> {
        let k = k.as_deref()?.trim();
        let s = s.as_deref()?.trim();
        if k.is_empty() || s.is_empty() {
            return None;
        }
        Some((k.to_string(), s.to_string()))
    }

    /// Point the market-data credentials at the selected account.
    ///
    /// Call this after anything that changes either the keys or the selected
    /// environment. `prefer_live` is the environment currently armed for trading.
    ///
    /// The fallback is deliberate: if the selected account has no keys yet — or the
    /// active broker is TagDash, which has no Alpaca account at all — we borrow the
    /// other pair rather than letting charts and the screener go dark. This picks a
    /// *data feed*; it never arms a trading account. That decision belongs to
    /// `broker::creds_for`, which only ever reads the pair matching its environment.
    ///
    /// With neither pair set we leave the existing values alone, so an install that
    /// predates the account fields keeps working until the user re-enters its keys.
    pub fn resolve_market_data(&mut self, prefer_live: bool) {
        let live = Self::pair(&self.alpaca_live_key, &self.alpaca_live_secret);
        let paper = Self::pair(&self.alpaca_paper_key, &self.alpaca_paper_secret);
        let chosen = if prefer_live {
            live.or(paper)
        } else {
            paper.or(live)
        };
        if let Some((k, s)) = chosen {
            self.alpaca_key = Some(k);
            self.alpaca_secret = Some(s);
        }
    }

    /// One-time migration from the era when TagDash asked for a standalone
    /// "market data" pair. Alpaca never issued such a thing: whatever the user
    /// pasted there is one of their two account pairs, so move it into the matching
    /// slot instead of making them type it a second time.
    ///
    /// Alpaca prefixes key ids by environment — `PK…` paper, `AK…` live — and that
    /// prefix is the only signal available offline. Anything unrecognised is treated
    /// as paper: a migration must never be what arms a real-money account.
    ///
    /// Mis-slotting is not dangerous either way, because the *host* decides whether
    /// money is real, not the key. `paper-api.alpaca.markets` cannot spend a cent
    /// whatever credentials it is handed; a key in the wrong slot fails to
    /// authenticate, it does not trade.
    fn migrate_legacy_pair(&mut self) {
        let already_split = Self::pair(&self.alpaca_paper_key, &self.alpaca_paper_secret).is_some()
            || Self::pair(&self.alpaca_live_key, &self.alpaca_live_secret).is_some();
        if already_split {
            return;
        }
        let Some((k, s)) = Self::pair(&self.alpaca_key, &self.alpaca_secret) else {
            return;
        };
        if k.to_uppercase().starts_with("AK") {
            self.alpaca_live_key = Some(k);
            self.alpaca_live_secret = Some(s);
        } else {
            self.alpaca_paper_key = Some(k);
            self.alpaca_paper_secret = Some(s);
        }
    }
}

/// Persist secrets to `<root>/user/secrets.toml`. Written by hand (Secrets is
/// deliberately NOT `Serialize`, so values can never be sent to the frontend by an
/// accidental derive). Called when the user edits keys in Settings → API Keys.
pub fn save(app_dir: &Path, s: &Secrets) -> Result<(), String> {
    fn esc(o: &Option<String>) -> String {
        o.as_deref()
            .unwrap_or("")
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
    }
    let path = crate::paths::secrets_file(app_dir);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let content = format!(
        "# TagDash — API secrets (managed from Settings → API Keys).\n\
         # NEVER sent to the frontend. Keep it out of version control.\n\n\
         # One key pair per Alpaca account. Each pair serves BOTH trading and\n\
         # market data — Alpaca issues no separate market-data key.\n\
         alpaca_paper_key    = \"{}\"\n\
         alpaca_paper_secret = \"{}\"\n\
         alpaca_live_key     = \"{}\"\n\
         alpaca_live_secret  = \"{}\"\n\n\
         # Derived, do not edit: a copy of the selected account's pair, rewritten\n\
         # whenever the keys or the selected account change.\n\
         alpaca_key          = \"{}\"\n\
         alpaca_secret       = \"{}\"\n\n\
         fmp_api_key         = \"{}\"\n\
         massive_api_key     = \"{}\"\n\
         sec_api_key         = \"{}\"\n\
         claude_api_key      = \"{}\"\n\
         deepseek_api_key    = \"{}\"\n\n\
         # Open-web search, last resort of the Panic Score context cascade.\n\
         # Tavily wins when both are set. Never called during a Market Replay.\n\
         tavily_api_key      = \"{}\"\n\
         brave_api_key       = \"{}\"\n\n\
         tradetally_token    = \"{}\"\n\
         tradetally_email    = \"{}\"\n\
         tradetally_password = \"{}\"\n\n\
         # ntfy push (phone alerts). The TOPIC is the access control on the public\n\
         # server: anyone who knows it can read and publish to it. Server and token\n\
         # are optional — a public ntfy.sh topic needs neither.\n\
         ntfy_server         = \"{}\"\n\
         ntfy_topic          = \"{}\"\n\
         ntfy_token          = \"{}\"\n",
        esc(&s.alpaca_paper_key),
        esc(&s.alpaca_paper_secret),
        esc(&s.alpaca_live_key),
        esc(&s.alpaca_live_secret),
        esc(&s.alpaca_key),
        esc(&s.alpaca_secret),
        esc(&s.fmp_api_key),
        esc(&s.massive_api_key),
        esc(&s.sec_api_key),
        esc(&s.claude_api_key),
        esc(&s.deepseek_api_key),
        esc(&s.tavily_api_key),
        esc(&s.brave_api_key),
        esc(&s.tradetally_token),
        esc(&s.tradetally_email),
        esc(&s.tradetally_password),
        esc(&s.ntfy_server),
        esc(&s.ntfy_topic),
        esc(&s.ntfy_token),
    );
    std::fs::write(&path, content).map_err(|e| e.to_string())
}

/// Load secrets from `<root>/user/secrets.toml`.
/// If the file is absent or unreadable, return empty defaults.
/// Creates a template file on first run.
pub fn load(app_dir: &Path) -> Secrets {
    let path = crate::paths::secrets_file(app_dir);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }

    if !path.exists() {
        let template = r#"# TagDash — API secrets
# Edit this file to configure your API keys.
# This file is NEVER sent to the frontend.
# Keep it out of version control.

# Alpaca — one key pair per account, generated in the Alpaca dashboard.
# Each pair authenticates BOTH trading and market data; there is no separate
# market-data key. Which account is used is chosen in Settings → Broker.
alpaca_paper_key    = ""
alpaca_paper_secret = ""
alpaca_live_key     = ""
alpaca_live_secret  = ""
# FMP is kept for fallback/legacy; Massive is the active float provider.
fmp_api_key      = ""
# Massive (api.massive.com) — bulk free-float data.
massive_api_key  = ""
# sec-api.io — company country of origin + SIC industry.
sec_api_key      = ""
claude_api_key   = ""
# Deepseek (api.deepseek.com) — micro_pullback news/dilution analysis, and the
# Panic Score pre-open watchlist analysis (deepseek-v4-pro).
deepseek_api_key = ""

# Open-web search — the LAST resort of the Panic Score context cascade
# (Alpaca news → Massive news → web), used only when no structured source
# explains a move. Never called during a Market Replay. Set ONE of the two;
# Tavily wins if both are present.
tavily_api_key   = ""
brave_api_key    = ""

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
        Ok(content) => {
            let mut s = toml::from_str::<Secrets>(&content).unwrap_or_default();
            s.migrate_legacy_pair();
            s
        }
        Err(_) => Secrets::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_pairs(paper: Option<(&str, &str)>, live: Option<(&str, &str)>) -> Secrets {
        let mut s = Secrets::default();
        if let Some((k, v)) = paper {
            s.alpaca_paper_key = Some(k.into());
            s.alpaca_paper_secret = Some(v.into());
        }
        if let Some((k, v)) = live {
            s.alpaca_live_key = Some(k.into());
            s.alpaca_live_secret = Some(v.into());
        }
        s
    }

    #[test]
    fn market_data_follows_the_selected_account() {
        let mut s = with_pairs(Some(("PKpaper", "sp")), Some(("AKlive", "sl")));
        s.resolve_market_data(false);
        assert_eq!(s.alpaca_key.as_deref(), Some("PKpaper"));
        s.resolve_market_data(true);
        assert_eq!(s.alpaca_key.as_deref(), Some("AKlive"));
    }

    #[test]
    fn market_data_falls_back_to_the_other_account() {
        // Live selected but only the paper pair filled in: charts must not go dark.
        let mut s = with_pairs(Some(("PKpaper", "sp")), None);
        s.resolve_market_data(true);
        assert_eq!(s.alpaca_key.as_deref(), Some("PKpaper"));
        assert_eq!(s.alpaca_secret.as_deref(), Some("sp"));
    }

    #[test]
    fn a_half_filled_pair_is_not_a_pair() {
        let mut s = Secrets {
            alpaca_paper_key: Some("PKonly".into()),
            ..Default::default()
        };
        s.resolve_market_data(false);
        assert_eq!(
            s.alpaca_key, None,
            "a key without its secret must not be adopted"
        );
    }

    #[test]
    fn legacy_pair_migrates_by_key_prefix() {
        let mut s = Secrets {
            alpaca_key: Some("AKlivekey".into()),
            alpaca_secret: Some("sec".into()),
            ..Default::default()
        };
        s.migrate_legacy_pair();
        assert_eq!(s.alpaca_live_key.as_deref(), Some("AKlivekey"));
        assert_eq!(s.alpaca_paper_key, None);
    }

    #[test]
    fn an_unrecognised_legacy_prefix_lands_in_paper() {
        // Never let a migration be what arms a real-money account.
        let mut s = Secrets {
            alpaca_key: Some("mystery".into()),
            alpaca_secret: Some("sec".into()),
            ..Default::default()
        };
        s.migrate_legacy_pair();
        assert_eq!(s.alpaca_paper_key.as_deref(), Some("mystery"));
        assert_eq!(s.alpaca_live_key, None);
    }

    #[test]
    fn migration_never_overwrites_keys_the_user_entered() {
        let mut s = with_pairs(Some(("PKmine", "mine")), None);
        s.alpaca_key = Some("AKstale".into());
        s.alpaca_secret = Some("stale".into());
        s.migrate_legacy_pair();
        assert_eq!(s.alpaca_paper_key.as_deref(), Some("PKmine"));
        assert_eq!(s.alpaca_live_key, None);
    }

    #[test]
    fn save_round_trips_every_alpaca_account_field() {
        // Regression: the account pairs were applied in memory but dropped by save(),
        // so they silently vanished on the next launch.
        let dir = std::env::temp_dir().join(format!("tagdash-secrets-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let s = with_pairs(Some(("PKp", "sp")), Some(("AKl", "sl")));
        save(&dir, &s).unwrap();
        let back = load(&dir);
        assert_eq!(back.alpaca_paper_key.as_deref(), Some("PKp"));
        assert_eq!(back.alpaca_paper_secret.as_deref(), Some("sp"));
        assert_eq!(back.alpaca_live_key.as_deref(), Some("AKl"));
        assert_eq!(back.alpaca_live_secret.as_deref(), Some("sl"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod persistence_tests {
    use super::*;

    /// Unique scratch directory per test run, cleaned up by the caller.
    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "tagdash-secrets-{name}-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    /// Every field set to a distinct, recognisable value.
    fn fully_populated() -> Secrets {
        Secrets {
            alpaca_key: Some("v-alpaca_key".into()),
            alpaca_secret: Some("v-alpaca_secret".into()),
            alpaca_paper_key: Some("PKv-paper_key".into()),
            alpaca_paper_secret: Some("v-paper_secret".into()),
            alpaca_live_key: Some("AKv-live_key".into()),
            alpaca_live_secret: Some("v-live_secret".into()),
            fmp_api_key: Some("v-fmp".into()),
            massive_api_key: Some("v-massive".into()),
            sec_api_key: Some("v-sec".into()),
            claude_api_key: Some("v-claude".into()),
            deepseek_api_key: Some("v-deepseek".into()),
            tavily_api_key: Some("v-tavily".into()),
            brave_api_key: Some("v-brave".into()),
            tradetally_token: Some("tt_live_v".into()),
            tradetally_email: Some("v@example.com".into()),
            tradetally_password: Some("v-password".into()),
            ntfy_server: Some("https://push.example.com".into()),
            ntfy_topic: Some("v-topic".into()),
            ntfy_token: Some("tk_v".into()),
        }
    }

    /// THE guard on `save`. That writer hand-builds its TOML from a fixed list of
    /// fields — `Secrets` is deliberately not `Serialize`, so nothing makes the list
    /// track the struct. A field added to `Secrets` and forgotten there is written
    /// nowhere and silently lost on the next restart, with the UI still cheerfully
    /// reporting the key as configured. (That is exactly how the ntfy topic failed to
    /// persist.) Round-tripping every field through the real file catches it.
    #[test]
    fn every_secret_survives_a_save_and_reload() {
        let dir = scratch("roundtrip");
        let original = fully_populated();
        save(&dir, &original).expect("save");
        let loaded = load(&dir);

        // Field by field, so a failure names the field that was dropped.
        let pairs: [(&str, &Option<String>, &Option<String>); 19] = [
            ("alpaca_key", &original.alpaca_key, &loaded.alpaca_key),
            (
                "alpaca_secret",
                &original.alpaca_secret,
                &loaded.alpaca_secret,
            ),
            (
                "alpaca_paper_key",
                &original.alpaca_paper_key,
                &loaded.alpaca_paper_key,
            ),
            (
                "alpaca_paper_secret",
                &original.alpaca_paper_secret,
                &loaded.alpaca_paper_secret,
            ),
            (
                "alpaca_live_key",
                &original.alpaca_live_key,
                &loaded.alpaca_live_key,
            ),
            (
                "alpaca_live_secret",
                &original.alpaca_live_secret,
                &loaded.alpaca_live_secret,
            ),
            ("fmp_api_key", &original.fmp_api_key, &loaded.fmp_api_key),
            (
                "massive_api_key",
                &original.massive_api_key,
                &loaded.massive_api_key,
            ),
            ("sec_api_key", &original.sec_api_key, &loaded.sec_api_key),
            (
                "claude_api_key",
                &original.claude_api_key,
                &loaded.claude_api_key,
            ),
            (
                "deepseek_api_key",
                &original.deepseek_api_key,
                &loaded.deepseek_api_key,
            ),
            (
                "tavily_api_key",
                &original.tavily_api_key,
                &loaded.tavily_api_key,
            ),
            (
                "brave_api_key",
                &original.brave_api_key,
                &loaded.brave_api_key,
            ),
            (
                "tradetally_token",
                &original.tradetally_token,
                &loaded.tradetally_token,
            ),
            (
                "tradetally_email",
                &original.tradetally_email,
                &loaded.tradetally_email,
            ),
            (
                "tradetally_password",
                &original.tradetally_password,
                &loaded.tradetally_password,
            ),
            ("ntfy_server", &original.ntfy_server, &loaded.ntfy_server),
            ("ntfy_topic", &original.ntfy_topic, &loaded.ntfy_topic),
            ("ntfy_token", &original.ntfy_token, &loaded.ntfy_token),
        ];
        for (name, want, got) in pairs {
            assert_eq!(got, want, "`{name}` did not survive save → load");
        }

        // And the count is pinned: adding a field to `Secrets` without adding it to
        // the list above (and therefore to `save`) fails here rather than in the wild.
        let written = std::fs::read_to_string(crate::paths::secrets_file(&dir)).unwrap();
        let assignments = written
            .lines()
            .filter(|l| !l.trim_start().starts_with('#') && l.contains(" = "))
            .count();
        assert_eq!(
            assignments,
            pairs.len(),
            "`save` writes {assignments} fields but {} are checked — a field is missing \
             from the writer or from this test:\n{written}",
            pairs.len(),
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The status the UI reads must agree with what is on disk, or the API Keys tab
    /// shows "configuré" for a key that was never written.
    #[test]
    fn the_status_reported_to_the_ui_matches_what_was_persisted() {
        let dir = scratch("status");
        save(&dir, &fully_populated()).expect("save");
        let st = load(&dir).status();
        assert!(st.ntfy_topic, "the topic must read back as configured");
        assert!(st.ntfy_server);
        assert!(st.ntfy_token);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A blank input must never wipe a stored secret — the whole reason the update is
    /// partial. Regression guard for the ntfy fields specifically.
    #[test]
    fn a_blank_update_leaves_an_existing_topic_alone() {
        let mut s = fully_populated();
        s.apply_update(SecretsUpdate {
            ntfy_topic: Some("   ".into()),
            ntfy_token: None,
            ..Default::default()
        });
        assert_eq!(s.ntfy_topic.as_deref(), Some("v-topic"));
        assert_eq!(s.ntfy_token.as_deref(), Some("tk_v"));

        s.apply_update(SecretsUpdate {
            ntfy_topic: Some("  nouveau-topic  ".into()),
            ..Default::default()
        });
        assert_eq!(
            s.ntfy_topic.as_deref(),
            Some("nouveau-topic"),
            "and is trimmed"
        );
    }
}
