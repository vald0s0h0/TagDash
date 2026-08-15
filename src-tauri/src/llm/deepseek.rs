// Deepseek LLM client (OpenAI-compatible chat completions). Used by the alert
// enrichment for short French reads: micro_pullback's news bluff/solid + dilution
// risk, and panic_mean_reversion's on-demand context + mean-reversion verdict.
// Strictly async; never blocks the hot path. When no API key is configured the
// caller simply skips these calls (wiring stays intact).
//
// TWO ENTRY POINTS, on purpose:
//   • `complete` / `complete_with_temperature` — the original short-text reads. They
//     stay on V4 Flash and their request shape is unchanged, so the two existing
//     callers cannot regress.
//   • `chat(Request)` — the configurable path (model, thinking budget, JSON mode,
//     token cap) added for the Panic Score watchlist analysis, which needs V4 Pro,
//     strict JSON, and the token usage back so prompt-cache hits are observable.

use serde::Deserialize;
use serde_json::json;

const API_URL: &str = "https://api.deepseek.com/chat/completions";
/// Deepseek V4 Flash — fast, cheap chat model (the account's available chat model;
/// the older "deepseek-chat" id is no longer served).
const MODEL: &str = "deepseek-v4-flash";
/// Deepseek V4 Pro — the quality-focused sibling. Used for the Panic Score analysis,
/// where a mis-scored ticker costs far more than the price difference: at ~4k input
/// tokens per ticker the whole watchlist is a few cents, and the invariant rubric
/// prefix is served from the prompt cache at $0.003625/M instead of $0.435/M.
pub const MODEL_PRO: &str = "deepseek-v4-pro";
/// V4 Flash is a REASONING model: it spends a large hidden `reasoning_content`
/// budget (often 150–350 tokens) BEFORE the visible answer. The token cap covers
/// both, so it must be well above the short answer length or the reply is
/// truncated mid-sentence (finish_reason="length"). 1500 leaves ample headroom.
const MAX_TOKENS: u32 = 1500;

/// Token accounting from one completion. `cache_hit` is the interesting one: it is
/// the direct evidence that the frozen rubric prefix is byte-identical across calls,
/// which is what makes the scores comparable in the first place. A run where it
/// stays 0 after the first ticker means the prefix drifted.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
pub struct Usage {
    #[serde(default, rename = "prompt_tokens")]
    pub prompt: u32,
    #[serde(default, rename = "completion_tokens")]
    pub completion: u32,
    #[serde(default, rename = "prompt_cache_hit_tokens")]
    pub cache_hit: u32,
    #[serde(default, rename = "prompt_cache_miss_tokens")]
    pub cache_miss: u32,
}

/// How hard the model should think before answering. Maps to Deepseek's
/// `reasoning_effort`; `None` disables thinking entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effort {
    Low,
    Medium,
    High,
}

impl Effort {
    fn as_str(self) -> &'static str {
        match self {
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
        }
    }
}

/// One configurable completion.
#[derive(Debug, Clone)]
pub struct Request<'a> {
    pub model: &'a str,
    pub system: &'a str,
    pub user: &'a str,
    /// `Some(effort)` enables thinking mode; `None` asks for the non-thinking path.
    pub effort: Option<Effort>,
    /// Ask for `response_format: {"type":"json_object"}`. Deepseek additionally
    /// requires the word "json" and an example somewhere in the prompt — the caller
    /// owns that (see `crate::panic_llm::prompt`).
    pub json: bool,
    pub max_tokens: u32,
    /// Sent for the record; Deepseek does not guarantee it is honoured in thinking
    /// mode, so determinism must never depend on it.
    pub temperature: f32,
}

/// A completion's visible answer plus its token accounting.
#[derive(Debug, Clone)]
pub struct Answer {
    pub content: String,
    pub usage: Usage,
}

pub struct Deepseek {
    api_key: String,
}

impl Deepseek {
    pub fn new(api_key: String) -> Self {
        Self { api_key }
    }

    /// The configurable path. Returns the visible message text (never the hidden
    /// `reasoning_content`) plus usage.
    pub async fn chat(&self, req: Request<'_>) -> Result<Answer, String> {
        let mut body = json!({
            "model": req.model,
            "messages": [
                { "role": "system", "content": req.system },
                { "role": "user",   "content": req.user },
            ],
            "temperature": req.temperature,
            "max_tokens": req.max_tokens,
            "stream": false,
        });
        match req.effort {
            Some(e) => {
                body["thinking"] = json!({ "type": "enabled" });
                body["reasoning_effort"] = json!(e.as_str());
            }
            None => body["thinking"] = json!({ "type": "disabled" }),
        }
        if req.json {
            body["response_format"] = json!({ "type": "json_object" });
        }

        // `llm_client`, not `client`: a thinking-model reply takes far longer than the
        // 30 s market-data budget, which silently truncated every one of these.
        let resp = crate::http::llm_client()
            .post(API_URL)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&body)
            .send()
            .await
            .map_err(|e| decode_hint(&e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("Deepseek HTTP {status}: {}", snippet(&text)));
        }

        let v: serde_json::Value = resp.json().await.map_err(|e| decode_hint(&e))?;
        // A truncated reply is a DIFFERENT failure from an empty one and needs a
        // different fix (raise max_tokens vs retry), so it is reported as such
        // rather than surfacing as unparseable JSON downstream.
        let finish = v["choices"][0]["finish_reason"].as_str().unwrap_or("");
        let content = v["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or("")
            .trim()
            .to_string();
        if finish == "length" {
            return Err(format!(
                "Deepseek: réponse tronquée (max_tokens={}) — augmenter le budget",
                req.max_tokens
            ));
        }
        if content.is_empty() {
            return Err("Deepseek: empty response".into());
        }
        let usage: Usage = serde_json::from_value(v["usage"].clone()).unwrap_or_default();
        Ok(Answer { content, usage })
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
        // Same reasoning as `chat`: V4 Flash also thinks before answering, so these
        // reads belong on the LLM budget rather than the market-data one. A timeout
        // here is silently discarded by both callers, which is exactly how a slow
        // reply would look like "the model had nothing to say".
        let client = crate::http::llm_client();
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
            .map_err(|e| decode_hint(&e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("Deepseek HTTP {status}: {}", snippet(&text)));
        }

        let v: serde_json::Value = resp.json().await.map_err(|e| decode_hint(&e))?;
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

/// Turn a reqwest failure into a message that names its own cause.
///
/// Reqwest reports a timeout that lands mid-body as the bare string "error decoding
/// response body", which reads like malformed JSON from the server and sent this
/// feature's diagnosis in entirely the wrong direction. `is_timeout()` distinguishes
/// the two, so the message can say what to actually change.
fn decode_hint(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        return format!(
            "Deepseek: délai dépassé ({e}) — la réponse a été coupée. \
             Voir http::LLM_TIMEOUT."
        );
    }
    if e.is_decode() {
        return format!("Deepseek: réponse illisible ({e}) — corps tronqué ou non-json.");
    }
    format!("Deepseek: {e}")
}

/// First 200 CHARACTERS of an error body. Character-based, not byte-based: the previous
/// `&text[..200]` would panic outright if byte 200 fell inside a multi-byte character,
/// turning a server-side error into a crash in the code meant to report it.
fn snippet(text: &str) -> String {
    const MAX: usize = 200;
    if text.chars().count() <= MAX {
        return text.trim().to_string();
    }
    let cut: String = text.chars().take(MAX).collect();
    format!("{}…", cut.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A multi-byte body must not panic the error path. Deepseek's messages are ASCII
    /// today, but an upstream proxy returning a French or CJK error page would have
    /// crashed the old byte-slice version.
    #[test]
    fn snippet_cuts_on_character_boundaries() {
        let accented = "é".repeat(300); // 2 bytes each — byte 200 is mid-character
        let out = snippet(&accented);
        assert_eq!(out.chars().count(), 201, "200 chars plus the ellipsis");
        assert!(out.ends_with('…'));
        assert_eq!(snippet("  court  "), "court");
        // Exactly at the cap: no ellipsis, no panic.
        let exact = "a".repeat(200);
        assert_eq!(snippet(&exact), exact);
    }
}
