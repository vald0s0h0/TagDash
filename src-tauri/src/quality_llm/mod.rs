// Quality LLM — the A+ scoring layer behind Micro Pullback's `quality_aplus` option.
//
// Two passes over one frozen scale (see `prompt`):
//   • TRIAGE  — deepseek-v4-flash, thinking disabled, ~2-4 s. It produces the fast
//               first opinion and is persisted even when the second pass is enabled.
//   • DEEP    — deepseek-v4-pro at medium effort, ~10-25 s, with filings, financials
//               and dilution attached. When selected, its score REPLACES triage and
//               is the blocking alert decision.
//
// The inverse of `panic_llm`: that one hunts moves that should not have happened, to
// fade. This one hunts moves with something real behind them, to join. The two share
// no rubric, no anchors and no table.
//
// DETERMINISTIC ADJUSTMENTS LIVE HERE, NOT IN THE MODEL. Hot sector, risk domicile
// and an armed ATM move the score by fixed amounts applied in `adjust`, and the
// rubrics explicitly tell the model not to apply them itself — otherwise they would
// count twice. Every term is recorded in `Adjustments` so a miss is auditable after
// the fact rather than being buried inside a single number.

pub mod payload;
pub mod prompt;
pub mod rubric;

use serde_json::Value;

use crate::llm::deepseek::{Deepseek, Effort, Request, MODEL_PRO};

pub use payload::QualityPayload;
pub use prompt::RUBRIC_VERSION;

/// Fast model for the triage pass. The deep pass uses `MODEL_PRO`.
const MODEL_FLASH: &str = "deepseek-v4-flash";

// ─── Deterministic score adjustments ───────────────────────────────────────────
// The SETUP terms — hot industry, risk domicile, exceptional RVOL, detached
// attention, tempo depth — are computed by `micro_pullback::preselect`, which owns
// their weights because it is also the gate that reads them. Only the ATM term lives
// here: it needs the deep pass's dossier, which does not exist when the gate runs.
/// An active ATM on a company with under two quarters of cash is an issuer that NEEDS
/// to sell into strength. The same ATM on a funded company is a dormant facility and
/// costs nothing — which is why both halves are required.
pub const PENALTY_ARMED_ATM: f64 = -10.0;
/// Cash runway (in quarters) below which an ATM counts as armed.
pub const ARMED_ATM_RUNWAY_QUARTERS: f64 = 2.0;

/// Which pass produced a score.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pass {
    Triage,
    Deep,
}

impl Pass {
    pub fn as_str(self) -> &'static str {
        match self {
            Pass::Triage => "triage",
            Pass::Deep => "deep",
        }
    }
}

/// One model answer, in the union of both passes' shapes. Triage-only calls leave the
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

/// Every term that moved the model's score, kept separately so a rejected candidate
/// can be explained without re-running anything.
///
/// `setup` is what the deterministic gate found before the call; `armed_atm` is the
/// one term only the deep pass can know. Splitting them is not cosmetic: with the
/// deep pass off, `armed_atm` is structurally zero, and a reader has to be able to
/// tell "no armed ATM" from "we never looked".
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Adjustments {
    pub setup: crate::micro_pullback::preselect::SetupTerms,
    pub armed_atm: f64,
}

impl Adjustments {
    pub fn total(&self) -> f64 {
        self.setup.total() + self.armed_atm
    }

    /// One-line audit trail for the log and the alert tooltip.
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if self.setup.hot_industry != 0.0 {
            parts.push(format!("secteur porteur {:+.0}", self.setup.hot_industry));
        }
        if self.setup.risk_domicile != 0.0 {
            parts.push(format!(
                "juridiction à risque {:+.0}",
                self.setup.risk_domicile
            ));
        }
        if self.setup.exceptional_rvol != 0.0 {
            parts.push(format!("RVOL exceptionnel {:+.0}", self.setup.exceptional_rvol));
        }
        if self.setup.market_attention != 0.0 {
            parts.push(format!(
                "attention détachée {:+.0}",
                self.setup.market_attention
            ));
        }
        if self.setup.tempo_depth != 0.0 {
            parts.push(format!("profondeur tempo {:+.0}", self.setup.tempo_depth));
        }
        if self.setup.recent_split != 0.0 {
            parts.push(format!("split récent {:+.0}", self.setup.recent_split));
        }
        if self.armed_atm != 0.0 {
            parts.push(format!("ATM armé {:+.0}", self.armed_atm));
        }
        if parts.is_empty() {
            "aucun ajustement".to_string()
        } else {
            parts.join(" · ")
        }
    }
}

/// The model's score plus the deterministic terms, clamped to the scale.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FinalScore {
    pub model: f64,
    pub adjustments: Adjustments,
    pub final_score: f64,
}

/// Apply the deterministic adjustments to a model score.
///
/// `setup` comes from the pre-selection gate and is already decided by the time the
/// model is called — it is passed in rather than recomputed so the number the gate
/// logged and the number in the final score can never disagree.
///
/// `cash_runway_quarters` is `None` when the financials aren't known — which is the
/// normal case on the triage pass, and the permanent case with the deep pass off. An
/// unknown runway is NOT treated as short: we don't penalise a company for a gap in
/// our own data, so the ATM term only fires when both halves are actually known.
pub fn adjust(
    model_score: f64,
    setup: crate::micro_pullback::preselect::SetupTerms,
    atm_active: Option<bool>,
    cash_runway_quarters: Option<f64>,
) -> FinalScore {
    let armed_atm = matches!(atm_active, Some(true))
        && matches!(cash_runway_quarters, Some(r) if r < ARMED_ATM_RUNWAY_QUARTERS);
    let adjustments = Adjustments {
        setup,
        armed_atm: if armed_atm { PENALTY_ARMED_ATM } else { 0.0 },
    };
    FinalScore {
        model: model_score,
        adjustments,
        final_score: (model_score + adjustments.total()).clamp(0.0, 100.0),
    }
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
    let payload = match pass {
        Pass::Triage => full_payload.for_triage(),
        Pass::Deep => full_payload.clone(),
    };
    let user = prompt::score_user_message(symbol, &payload.to_json());
    let client = Deepseek::new(api_key.to_string());

    // The rubric comes from its editable file, falling back to the compiled-in text.
    // Read here rather than cached so an edit lands on the very next candidate.
    let system = rubric::text(pass);
    let (model, effort, max_tokens) = match pass {
        Pass::Triage => (MODEL_FLASH, None, prompt::TRIAGE_MAX_TOKENS),
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

    // Both spellings are accepted on one scale. The triage rubric asks for
    // `breaking_score` — the name the trader sees on screen and in the editable
    // prompt file — while the deep rubric keeps `quality_score`. Reading either here
    // means an edited prompt that keeps the old key still works, and a model that
    // answers with the other name is not thrown away over a label.
    let quality_score = ["breaking_score", "quality_score"]
        .iter()
        .find_map(|k| v.get(*k).and_then(Value::as_f64))
        .ok_or_else(|| "breaking_score/quality_score absent ou non numérique".to_string())?;

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

    use crate::micro_pullback::preselect::{
        SetupTerms, BONUS_HOT_INDUSTRY, PENALTY_RISK_DOMICILE,
    };

    /// The two issuer terms the pre-selection gate hands over. Named through the
    /// gate's own constants so a reweighting there breaks these on purpose.
    fn setup(hot: bool, risky: bool) -> SetupTerms {
        SetupTerms {
            hot_industry: if hot { BONUS_HOT_INDUSTRY } else { 0.0 },
            risk_domicile: if risky { PENALTY_RISK_DOMICILE } else { 0.0 },
            ..Default::default()
        }
    }

    #[test]
    fn a_plain_issuer_is_scored_exactly_as_the_model_said() {
        let f = adjust(72.0, setup(false, false), Some(false), Some(8.0));
        assert_eq!(f.final_score, 72.0);
        assert_eq!(f.adjustments, Adjustments::default());
        assert_eq!(f.adjustments.describe(), "aucun ajustement");
    }

    #[test]
    fn hot_industry_lifts_and_risk_domicile_sinks() {
        assert_eq!(adjust(70.0, setup(true, false), None, None).final_score, 85.0);
        assert_eq!(adjust(70.0, setup(false, true), None, None).final_score, 45.0);
        // A Chinese biotech gets both: the tailwind does not cancel the base rate,
        // and the base rate is the heavier of the two.
        let both = adjust(70.0, setup(true, true), None, None);
        assert_eq!(both.final_score, 60.0);
        assert!(both.adjustments.describe().contains("secteur porteur +15"));
        assert!(both
            .adjustments
            .describe()
            .contains("juridiction à risque -25"));
    }

    #[test]
    fn every_setup_term_reaches_the_final_score_and_the_audit_line() {
        let all = SetupTerms {
            hot_industry: 15.0,
            risk_domicile: 0.0,
            exceptional_rvol: 10.0,
            market_attention: 5.0,
            tempo_depth: 5.0,
            recent_split: 0.0,
        };
        let f = adjust(50.0, all, None, None);
        assert_eq!(f.final_score, 85.0);
        let d = f.adjustments.describe();
        for term in [
            "secteur porteur +15",
            "RVOL exceptionnel +10",
            "attention détachée +5",
            "profondeur tempo +5",
        ] {
            assert!(d.contains(term), "{term} missing from: {d}");
        }
    }

    /// An ATM only matters when the issuer actually needs to use it. Both halves are
    /// required, which is the whole point of the term.
    #[test]
    fn an_atm_is_only_penalised_when_the_cash_is_short() {
        let p = setup(false, false);
        assert_eq!(
            adjust(80.0, p, Some(true), Some(1.0)).final_score,
            70.0,
            "armed"
        );
        assert_eq!(
            adjust(80.0, p, Some(true), Some(9.0)).final_score,
            80.0,
            "funded"
        );
        assert_eq!(
            adjust(80.0, p, Some(false), Some(1.0)).final_score,
            80.0,
            "no ATM"
        );
    }

    /// The triage pass has no financials, and with the deep pass off nothing ever
    /// will. An unknown runway must not be read as a short one — we never penalise a
    /// ticker for a gap in our own data.
    #[test]
    fn an_unknown_runway_never_arms_the_atm_penalty() {
        let f = adjust(80.0, setup(false, false), Some(true), None);
        assert_eq!(f.final_score, 80.0);
        assert_eq!(f.adjustments.armed_atm, 0.0);
    }

    #[test]
    fn the_final_score_stays_on_the_scale() {
        assert_eq!(
            adjust(97.0, setup(true, false), None, None).final_score,
            100.0
        );
        assert_eq!(
            adjust(5.0, setup(false, true), Some(true), Some(0.5)).final_score,
            0.0
        );
    }

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
    fn deep_only_fields_are_absent_on_a_triage_reply() {
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
