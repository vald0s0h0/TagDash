// Online cleanup of a raw transcription through Deepseek (reuses llm::deepseek). When
// offline / no key / call fails, we pass the raw whisper text through unchanged (and
// no diary title) — the failed call IS the "offline" signal, so there is no separate
// connectivity check. The note is sent either way (the TradeTally queue handles the
// rest), so a dictée is never lost just because Deepseek was unreachable.

use std::sync::{Arc, RwLock};

use serde::Deserialize;

use crate::config::secrets::Secrets;
use crate::llm::deepseek::Deepseek;

use super::JobKind;

#[derive(Debug, Clone)]
pub struct Cleaned {
    pub text: String,
    /// Only set for diary notes, and only when Deepseek answered.
    pub title: Option<String>,
}

#[derive(Deserialize)]
struct DsReply {
    #[serde(default)]
    text: String,
    #[serde(default)]
    title: Option<String>,
}

const SYS_TRADE: &str = "Tu nettoies une note vocale de trading dictée en français. \
Corrige la ponctuation, les fautes et l'oralité, sans rien inventer, résumer ni traduire. \
Garde les termes de bourse anglais tels quels (halt, VWAP, float, breakout, ticker…). \
Réponds UNIQUEMENT par un JSON compact: {\"text\":\"<note corrigée>\"}";

const SYS_DIARY: &str = "Tu nettoies une note de journal de trading dictée en français. \
Corrige la ponctuation, les fautes et l'oralité, sans rien inventer, résumer ni traduire. \
Garde les termes de bourse anglais tels quels. Propose aussi un titre court (max 6 mots). \
Réponds UNIQUEMENT par un JSON compact: {\"title\":\"<titre>\",\"text\":\"<note corrigée>\"}";

/// Clean a raw transcription. Always returns something usable.
pub async fn clean(secrets: &Arc<RwLock<Secrets>>, kind: JobKind, raw: &str) -> Cleaned {
    let raw = raw.trim();
    let fallback = || Cleaned { text: raw.to_string(), title: None };

    let key = {
        let s = secrets.read().unwrap();
        s.deepseek_api_key.clone()
    };
    let Some(key) = key.filter(|k| !k.is_empty()) else {
        return fallback();
    };
    if raw.is_empty() {
        return fallback();
    }

    let system = match kind {
        JobKind::Trade => SYS_TRADE,
        JobKind::Diary => SYS_DIARY,
    };
    match Deepseek::new(key).complete(system, raw).await {
        Ok(resp) => parse_reply(&resp).unwrap_or_else(fallback),
        Err(e) => {
            eprintln!("[stt] deepseek cleanup failed (offline?): {e}");
            fallback()
        }
    }
}

/// Parse the model's JSON, tolerating stray prose around it.
fn parse_reply(resp: &str) -> Option<Cleaned> {
    let slice = {
        let start = resp.find('{')?;
        let end = resp.rfind('}')?;
        if end <= start {
            return None;
        }
        &resp[start..=end]
    };
    let parsed: DsReply = serde_json::from_str(slice).ok()?;
    let text = parsed.text.trim().to_string();
    if text.is_empty() {
        return None;
    }
    let title = parsed.title.map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
    Some(Cleaned { text, title })
}
