// HTTP client for the TradeTally REST API.
// All methods return quickly and never block the live scanner path.
// Mock mode simulates success/failure without network I/O.

use reqwest::Client;
use serde_json::{json, Value};
use std::time::Duration;

pub struct TtClient {
    inner:     Client,
    base_url:  String,
    token:     String,
    mock_mode: bool,
    mock_fail: bool,
    mock_delay_ms: u64,
    // Session login (email/password) — only for screenshot upload.
    email:     Option<String>,
    password:  Option<String>,
}

impl TtClient {
    pub fn new(base_url: String, token: String, mock_mode: bool) -> Self {
        Self {
            inner: crate::http::client(),
            base_url,
            token,
            mock_mode,
            mock_fail: false,
            mock_delay_ms: 0,
            email: None,
            password: None,
        }
    }

    pub fn with_mock_options(mut self, fail: bool, delay_ms: u64) -> Self {
        self.mock_fail = fail;
        self.mock_delay_ms = delay_ms;
        self
    }

    pub fn with_session_creds(mut self, email: Option<String>, password: Option<String>) -> Self {
        self.email = email;
        self.password = password;
        self
    }

    // ── Internal helpers ─────────────────────────────────────────────────────

    fn url(&self, endpoint: &str) -> String {
        format!("{}{}", self.base_url, endpoint)
    }

    fn auth_header(&self) -> String {
        format!("Bearer {}", self.token)
    }

    async fn mock_response(&self) -> Result<Value, String> {
        if self.mock_delay_ms > 0 {
            tokio::time::sleep(Duration::from_millis(self.mock_delay_ms)).await;
        }
        if self.mock_fail {
            return Err("mock: forced failure".into());
        }
        let mock_id = format!("mock-{}", chrono::Utc::now().timestamp_millis());
        Ok(json!({ "id": mock_id, "status": "ok" }))
    }

    async fn handle_resp(&self, resp: reqwest::Response) -> Result<Value, String> {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            let snippet = &text[..text.len().min(300)];
            return Err(format!("HTTP {status}: {snippet}"));
        }
        if text.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&text).map_err(|e| format!("JSON parse: {e}"))
    }

    // ── Public verbs ─────────────────────────────────────────────────────────

    pub async fn post_json(&self, endpoint: &str, body: &Value) -> Result<Value, String> {
        if self.mock_mode {
            return self.mock_response().await;
        }
        let resp = self.inner
            .post(self.url(endpoint))
            .header("Authorization", self.auth_header())
            .header("Content-Type", "application/json")
            .timeout(Duration::from_secs(15))
            .json(body)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        self.handle_resp(resp).await
    }

    pub async fn patch_json(&self, endpoint: &str, body: &Value) -> Result<Value, String> {
        if self.mock_mode {
            return self.mock_response().await;
        }
        let resp = self.inner
            .patch(self.url(endpoint))
            .header("Authorization", self.auth_header())
            .header("Content-Type", "application/json")
            .timeout(Duration::from_secs(15))
            .json(body)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        self.handle_resp(resp).await
    }

    pub async fn put_json(&self, endpoint: &str, body: &Value) -> Result<Value, String> {
        if self.mock_mode {
            return self.mock_response().await;
        }
        let resp = self.inner
            .put(self.url(endpoint))
            .header("Authorization", self.auth_header())
            .header("Content-Type", "application/json")
            .timeout(Duration::from_secs(15))
            .json(body)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        self.handle_resp(resp).await
    }

    /// GET a JSON resource (Bearer token). Used by the dashboard to page through
    /// `/api/v1/trades`. In mock mode returns an empty payload so the sync is a no-op.
    pub async fn get_json(&self, endpoint: &str) -> Result<Value, String> {
        if self.mock_mode {
            return Ok(json!({ "trades": [] }));
        }
        let resp = self.inner
            .get(self.url(endpoint))
            .header("Authorization", self.auth_header())
            .header("Accept", "application/json")
            .timeout(Duration::from_secs(20))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        self.handle_resp(resp).await
    }

    /// Create or update today's diary entry (`POST /api/diary`). That route sits
    /// behind the session `authenticate` middleware (not the flexible API-key auth
    /// the v1 trade routes use), so the API token may be rejected: try it first,
    /// then fall back to a session-login JWT (email/password) on 401/403.
    pub async fn create_diary_entry(
        &self,
        entry_date: &str,
        title: &str,
        content: &str,
    ) -> Result<(), String> {
        if self.mock_mode {
            return self.mock_response().await.map(|_| ());
        }
        let endpoint = "/api/diary";
        let body = json!({
            "entryDate": entry_date,
            "title":     title,
            "content":   content,
            "entryType": "diary",
        });

        // 1) Try the API token.
        let resp = self.inner
            .post(self.url(endpoint))
            .header("Authorization", self.auth_header())
            .header("Content-Type", "application/json")
            .timeout(Duration::from_secs(15))
            .json(&body)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let status = resp.status();
        if status.is_success() {
            return Ok(());
        }
        if status.as_u16() != 401 && status.as_u16() != 403 {
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("diary HTTP {status}: {}", &text[..text.len().min(200)]));
        }

        // 2) Token refused → session login, retry with the JWT.
        let jwt = self.login_jwt().await?;
        let resp = self.inner
            .post(self.url(endpoint))
            .header("Authorization", format!("Bearer {jwt}"))
            .header("Content-Type", "application/json")
            .timeout(Duration::from_secs(15))
            .json(&body)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("diary (session) HTTP {status}: {}", &text[..text.len().min(200)]));
        }
        Ok(())
    }

    /// Log in with the stored email/password and return a JWT. Shared by the
    /// diary POST and the screenshot upload (both behind session auth).
    async fn login_jwt(&self) -> Result<String, String> {
        let (email, password) = match (self.email.as_deref(), self.password.as_deref()) {
            (Some(e), Some(p)) if !e.is_empty() && !p.is_empty() => (e, p),
            _ => return Err(
                "TradeTally session credentials not set (tradetally_email / tradetally_password)".into()
            ),
        };
        let login = self.inner
            .post(format!("{}/api/auth/login", self.base_url))
            .header("Content-Type", "application/json")
            .timeout(Duration::from_secs(15))
            .json(&json!({ "email": email, "password": password }))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let login_status = login.status();
        let login_text = login.text().await.unwrap_or_default();
        if !login_status.is_success() {
            return Err(format!("login HTTP {login_status}: {}", &login_text[..login_text.len().min(160)]));
        }
        let lj: Value = serde_json::from_str(&login_text).map_err(|e| format!("login JSON: {e}"))?;
        lj.get("token").and_then(|v| v.as_str()).map(str::to_string).ok_or_else(|| {
            if lj.get("requires2FA").and_then(|v| v.as_bool()).unwrap_or(false) {
                "TradeTally login requires 2FA — session actions not supported".to_string()
            } else {
                "TradeTally login returned no token".to_string()
            }
        })
    }

    /// Upload a screenshot to a trade's image gallery.
    /// The TradeTally /images route needs a logged-in session (the API token is
    /// rejected there), so this logs in with email/password to obtain a JWT,
    /// then POSTs the PNG as multipart field `images` (matches the server's
    /// `upload.array('images', 10)`). `image_endpoint` is already {TT_ID}-resolved.
    pub async fn upload_images_session(&self, image_endpoint: &str, file_path: &str) -> Result<(), String> {
        if self.mock_mode {
            if self.mock_delay_ms > 0 {
                tokio::time::sleep(Duration::from_millis(self.mock_delay_ms)).await;
            }
            return if self.mock_fail { Err("mock: forced failure".into()) } else { Ok(()) };
        }

        // 1) Log in to obtain a JWT (top-level `token` field).
        let jwt = self.login_jwt().await?;

        // 2) Upload the PNG (multipart field `images`).
        let bytes = std::fs::read(file_path).map_err(|e| format!("read screenshot: {e}"))?;
        let filename = std::path::Path::new(file_path)
            .file_name().and_then(|f| f.to_str()).unwrap_or("chart.png").to_string();
        let part = reqwest::multipart::Part::bytes(bytes)
            .file_name(filename)
            .mime_str("image/png")
            .map_err(|e| e.to_string())?;
        let form = reqwest::multipart::Form::new().part("images", part);
        let resp = self.inner
            .post(self.url(image_endpoint))
            .header("Authorization", format!("Bearer {jwt}"))
            .timeout(Duration::from_secs(30))
            .multipart(form)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("image upload HTTP {status}: {}", &text[..text.len().min(200)]));
        }
        Ok(())
    }
}
