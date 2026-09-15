// Quality LLM — the model's third of the Micro Pullback score.
//
// Two passes (see `prompt`):
//   • NEWS SCORING — deepseek-v4-flash, thinking disabled, ~2-4 s. Grades the
//                    headline and NOTHING ELSE, and owns one third of the score.
//                    Always runs, budget permitting.
//   • DEEP         — deepseek-v4-pro at medium effort, ~10-25 s, with filings,
//                    financials and dilution attached. This pass, and only this
//                    pass, is what the "Qualité A+" switch turns on. Off by default.
//
// The inverse of `panic_llm`: that one hunts moves that should not have happened, to
// fade. This one hunts moves with something real behind them, to join. The two share
// no rubric, no anchors and no table.
//
// NO DETERMINISTIC ADJUSTMENTS ANY MORE. Hot sector, risk domicile, recent reverse
// split and an armed ATM used to move this score by fixed points. They are now
// either universe filters the user switches on, or context the trader reads on the
// pane. The score is three thirds — RVOL, attention, news — and nothing else.

pub mod payload;
pub mod prompt;
pub mod rubric;

use serde_json::Value;

use crate::llm::deepseek::{Deepseek, Effort, Request, MODEL_PRO};

pub use payload::QualityPayload;
pub use prompt::RUBRIC_VERSION;

/// Fast model for the news pass. The deep pass uses `MODEL_PRO`.
const MODEL_FLASH: &str = "deepseek-v4-flash";

/// Which pass produced a score.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pass {
    /// LLM NEWS SCORING: grades the headline, and only the headline. It used to be
    /// called "triage", which was a lie about what it does — it never triaged
    /// anything, it graded a catalyst.
    News,
    /// The optional second opinion, which is now the entirety of "Qualité A+".
    Deep,
}

impl Pass {
    pub fn as_str(self) -> &'static str {
        match self {
            Pass::News => "news",
            Pass::Deep => "deep",
        }
    }
}

/// One model answer, in the union of both passes' shapes. News-only calls leave the
/// deep-only fields None; the caller knows which pass it asked for.
#[derive(Debug, Clone, Default)]
pub struct QualityAnswer {
    pub quality_score: f64,
    pub confidence: Option<f64>,
    pub news_verdict: Option<String>,
    pub news_freshness: Option<String>,
    pub news_label: Option<String>,
    pub red_flags: Vec<String>,
    pub one_liner: Option<String>,
    // Deep pass only.
    pub sustain_prob: Option<f64>,
    pub catalyst_type: Option<String>,
    pub catalyst_label: Option<String>,
    pub dilution_risk: Option<String>,
    pub dilution_read: Option<String>,
    pub financial_read: Option<String>,
    pub key_drivers: Vec<String>,
    pub entry_note: Option<String>,
}

// ─── Calling the model ─────────────────────────────────────────────────────────

/// Score one candidate. `pass` selects the model, the reasoning budget and which
/// slice of the payload is sent.
///
/// Errors are returned, never swallowed: the caller decides whether a failed score
/// means "don't alert" (blocking mode) or "leave the alert as it is" (upgrade mode).
pub async fn score(
    api_key: &str,
    pass: Pass,
    symbol: &str,
    full_payload: &QualityPayload,
) -> Result<QualityAnswer, String> {
    // Two genuinely different documents: the news pass grades headlines and is sent
    // headlines; the deep pass grades a dossier and is sent one.
    let payload_json = match pass {
        Pass::News => full_payload.for_news().to_json(),
        Pass::Deep => full_payload.to_json(),
    };
    let user = prompt::score_user_message(symbol, &payload_json);
    let client = Deepseek::new(api_key.to_string());

    // The rubric comes from its editable file, falling back to the compiled-in text.
    // Read here rather than cached so an edit lands on the very next candidate.
    let system = rubric::text(pass);
    let (model, effort, max_tokens) = match pass {
        Pass::News => (MODEL_FLASH, None, prompt::NEWS_MAX_TOKENS),
        Pass::Deep => (MODEL_PRO, Some(Effort::Medium), prompt::DEEP_MAX_TOKENS),
    };
    let system = system.as_str();

    let req = Request {
        model,
        system,
        user: &user,
        effort,
        json: true,
        max_tokens,
        temperature: 0.0,
    };

    let answer = match client.chat(req.clone()).await {
        Ok(a) => a,
        Err(e) if e.contains("tronquée") => {
            // The one retry, and only on truncation: a reasoning budget that ran out
            // before the JSON was emitted. Anything else (auth, rate limit, network)
            // will fail again the same way, so retrying just doubles the latency the
            // trader is waiting on.
            let mut retry = req;
            retry.effort = Some(Effort::Low);
            client.chat(retry).await?
        }
        Err(e) => return Err(e),
    };

    parse_answer(&answer.content)
}

/// Parse a model reply. `quality_score` is REQUIRED: a reply without it is an error
/// rather than a zero, because a zero would silently read as "terrible candidate"
/// and be indistinguishable from a real rejection.
pub fn parse_answer(raw: &str) -> Result<QualityAnswer, String> {
    let json = extract_json(raw).ok_or_else(|| "réponse sans objet json".to_string())?;
    let v: Value = serde_json::from_str(json).map_err(|e| format!("json invalide: {e}"))?;

    // Every spelling is accepted on one scale. The news rubric asks for
    // `news_score` — the name the trader sees on screen and in the editable prompt
    // file — while the deep rubric keeps `quality_score`. `breaking_score` is the
    // previous name for the same field and is still accepted. Reading any of them
    // means an edited prompt that keeps the old key still works, and a model that
    // answers with the other name is not thrown away over a label.
    let quality_score = ["news_score", "breaking_score", "quality_score"]
        .iter()
        .find_map(|k| v.get(*k).and_then(Value::as_f64))
        .ok_or_else(|| "news_score/quality_score absent ou non numérique".to_string())?;

    Ok(QualityAnswer {
        quality_score: clamp_score(quality_score),
        confidence: v.get("confidence").and_then(Value::as_f64).map(clamp_score),
        news_verdict: string_field(&v, "news_verdict"),
        news_freshness: string_field(&v, "news_freshness"),
        news_label: string_field(&v, "news_label"),
        red_flags: string_list(&v, "red_flags", 4),
        one_liner: string_field(&v, "one_liner"),
        sustain_prob: v
            .get("sustain_prob")
            .and_then(Value::as_f64)
            .map(clamp_score),
        catalyst_type: string_field(&v, "catalyst_type"),
        catalyst_label: string_field(&v, "catalyst_label"),
        dilution_risk: string_field(&v, "dilution_risk"),
        dilution_read: string_field(&v, "dilution_read"),
        financial_read: string_field(&v, "financial_read"),
        key_drivers: string_list(&v, "key_drivers", 6),
        entry_note: string_field(&v, "entry_note"),
    })
}

/// Out-of-range numbers are clamped rather than rejected: a model that answers 105 is
/// telling us "as high as it goes", and refusing the whole answer over it would cost
/// the trader the alert.
fn clamp_score(v: f64) -> f64 {
    if v.is_finite() {
        v.clamp(0.0, 100.0)
    } else {
        0.0
    }
}

fn string_field(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn string_list(v: &Value, key: &str, cap: usize) -> Vec<String> {
    v.get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .take(cap)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Take the outermost `{...}`, so a code fence or a stray sentence around the object
/// is tolerated.
fn extract_json(raw: &str) -> Option<&str> {
    let start = raw.find('{')?;
    let end = raw.rfind('}')?;
    (end > start).then(|| &raw[start..=end])
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_reply_without_a_score_is_an_error_not_a_zero() {
        // A zero would be indistinguishable from a real rejection, and would silently
        // suppress an alert on a malformed reply.
        let err = parse_answer(r#"{"news_verdict":"solide","one_liner":"…"}"#).unwrap_err();
        assert!(err.contains("quality_score"), "{err}");
        assert!(parse_answer("pas du json du tout").is_err());
    }

    #[test]
    fn a_fenced_reply_is_still_read() {
        let raw = "Voici l'analyse :\n```json\n{\"quality_score\": 81, \
                   \"news_verdict\":\"solide\"}\n```\nVoilà.";
        let a = parse_answer(raw).unwrap();
        assert_eq!(a.quality_score, 81.0);
        assert_eq!(a.news_verdict.as_deref(), Some("solide"));
    }

    #[test]
    fn scores_are_clamped_and_blank_strings_drop_out() {
        let a = parse_answer(
            r#"{"quality_score":140,"confidence":-5,"news_label":"  ","red_flags":["a","","b"]}"#,
        )
        .unwrap();
        assert_eq!(a.quality_score, 100.0);
        assert_eq!(a.confidence, Some(0.0));
        assert_eq!(a.news_label, None, "a blank string is not a value");
        assert_eq!(a.red_flags, vec!["a", "b"]);
    }

    #[test]
    fn deep_only_fields_are_absent_on_a_news_reply() {
        let a = parse_answer(r#"{"quality_score":78,"news_verdict":"solide"}"#).unwrap();
        assert!(a.sustain_prob.is_none());
        assert!(a.dilution_risk.is_none());
        assert!(a.key_drivers.is_empty());
    }

    #[test]
    fn a_deep_reply_is_read_in_full() {
        let a = parse_answer(
            r#"{"quality_score":84,"sustain_prob":71,"confidence":76,
                "news_verdict":"solide","news_freshness":"breaking",
                "catalyst_type":"clinical","catalyst_label":"phase 2 positive",
                "dilution_risk":"moyen","dilution_read":"shelf dormant",
                "financial_read":"6 trimestres","red_flags":["shelf"],
                "key_drivers":["float 4M","SI 22%"],"one_liner":"…","entry_note":"sous le VWAP"}"#,
        )
        .unwrap();
        assert_eq!(a.quality_score, 84.0);
        assert_eq!(a.sustain_prob, Some(71.0));
        assert_eq!(a.dilution_risk.as_deref(), Some("moyen"));
        assert_eq!(a.key_drivers.len(), 2);
        assert_eq!(a.entry_note.as_deref(), Some("sous le VWAP"));
    }

    #[test]
    fn key_drivers_and_red_flags_are_capped() {
        let many: Vec<String> = (0..20).map(|i| format!("\"d{i}\"")).collect();
        let raw = format!(
            r#"{{"quality_score":50,"key_drivers":[{}],"red_flags":[{}]}}"#,
            many.join(","),
            many.join(",")
        );
        let a = parse_answer(&raw).unwrap();
        assert_eq!(a.key_drivers.len(), 6);
        assert_eq!(a.red_flags.len(), 4);
    }
}
