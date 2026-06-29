// Deepseek LLM client (OpenAI-compatible chat completions). Used by the alert
// enrichment for short French reads: micro_pullback's news bluff/solid + dilution
// risk, and panic_mean_reversion's on-demand context + mean-reversion verdict.
// Strictly async; never blocks the hot path. When no API key is configured the
// caller simply skips these calls (wiring stays intact).

use serde_json::json;

const API_URL: &str = "https://api.deepseek.com/chat/completions";
/// Deepseek V4 Flash — fast, cheap chat model (the account's available chat model;
/// the older "deepseek-chat" id is no longer served).
const MODEL: &str = "deepseek-v4-flash";
/// V4 Flash is a REASONING model: it spends a large hidden `reasoning_content`
/// budget (often 150–350 tokens) BEFORE the visible answer. The token cap covers
/// both, so it must be well above the short answer length or the reply is
/// truncated mid-sentence (finish_reason="length"). 1500 leaves ample headroom.
const MAX_TOKENS: u32 = 1500;

pub struct Deepseek {
    api_key: String,
}

impl Deepseek {
    pub fn new(api_key: String) -> Self {
        Self { api_key }
    }

    /// One chat completion at the default temperature (0.3).
    pub async fn complete(&self, system: &str, user: &str) -> Result<String, String> {
        self.complete_with_temperature(system, user, 0.3).await
    }

    /// One chat completion at an explicit temperature. Lower values (≈0.2) give a
    /// more deterministic, professional read. Returns the assistant message text.
    pub async fn complete_with_temperature(
        &self,
        system: &str,
        user: &str,
        temperature: f32,
    ) -> Result<String, String> {
        let client = crate::http::client();
        let body = json!({
            "model": MODEL,
            "messages": [
                { "role": "system", "content": system },
                { "role": "user",   "content": user },
            ],
            "temperature": temperature,
            "max_tokens": MAX_TOKENS,
            "stream": false,
        });

        let resp = client
            .post(API_URL)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&body)
            .send()
            .await
            .map_err(|e| e.to_string())?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            let snippet = &text[..text.len().min(200)];
            return Err(format!("Deepseek HTTP {status}: {snippet}"));
        }

        let v: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
        let content = v["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or("")
            .trim()
            .to_string();
        if content.is_empty() {
            return Err("Deepseek: empty response".into());
        }
        Ok(content)
    }
}
