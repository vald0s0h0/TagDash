// Issuer profile — the two "who is this company" reads the Micro Pullback A+
// qualification scores on, both derived from `company_meta` (country / sic /
// industry / sector, written by the startup pipeline from sec-api.io):
//
//   • hot industry  — sectors that structurally pull a premarket crowd
//                     (biotech, space, crypto, quantum, cannabis, AI): +8
//   • risk domicile — jurisdictions heavily over-represented in US small-cap
//                     pump & dump schemes (China, Hong Kong, Cayman, BVI): −15
//
// DUPLICATION, DELIBERATE. The hot-industry lists mirror
// `src/lib/industryCategories.ts` and the country normalisation mirrors
// `src/lib/countries.ts`. The frontend needs them to draw its chips without a
// round trip; the engine needs them to score without a frontend. Neither side can
// be the single source without making the other pay for it, so both exist —
// **change one, change the other**. The tests at the bottom pin the shared cases.

/// Exact SEC SIC titles (case-insensitive) belonging to a hot category. Sourced by
/// hand from `sec_api::sic_codes`; codes are kept in comments so a regenerated
/// table with reworded titles can be re-reconciled. SEC's own typos are preserved
/// verbatim — they are what the field actually contains.
const HOT_SIC_TITLES: &[&str] = &[
    // Biotechnology / pharma / life-sciences research
    "MEDICINAL CHEMICALS & BOTANICAL PRODUCTS", // 2833
    "PHARMACEUTICAL PREPARATIONS",              // 2834
    "IN VITRO & IN VIVO DIAGNOSTIC SUBSTANCES", // 2835
    "BIOLOGICAL PRODUCTS, (NO DISGNOSTIC SUBSTANCES)", // 2836 (sic)
    "SERVICES-COMMERCIAL PHYSICAL & BIOLOGICAL RESEARCH", // 8731
    // Space / aerospace / defence
    "GUIDED MISSILES & SPACE VEHICLES & PARTS", // 3760
    "SEARCH, DETECTION, NAVAGATION, GUIDANCE, AERONAUTICAL SYS", // 3812 (sic)
    "AIRCRAFT & PARTS",                         // 3720
    "AIRCRAFT",                                 // 3721
    "AIRCRAFT ENGINES & ENGINE PARTS",          // 3724
    "AIRCRAFT PARTS & AUXILIARY EQUIPMENT, NEC", // 3728
];

/// Free-text keyword fallback — the only path for categories the 1938-vintage SIC
/// schema has no code for (cannabis, crypto, quantum, AI), and a safety net for
/// biotech/space wording sec-api uses instead of the official SIC title.
/// Each entry maps to the bucket label shown to the trader and sent to the LLM.
const HOT_KEYWORDS: &[(&str, &str)] = &[
    ("biotech", "biotech"),
    ("biopharma", "biotech"),
    ("pharmaceutical", "biotech"),
    ("cannabis", "cannabis"),
    ("marijuana", "cannabis"),
    ("hemp", "cannabis"),
    ("crypto", "crypto"),
    ("bitcoin", "crypto"),
    ("blockchain", "crypto"),
    ("quantum", "quantum"),
    ("space", "space"),
    ("aerospace", "space"),
    ("artificial intelligence", "ia"),
];

/// Bucket for a SIC title that matched `HOT_SIC_TITLES`.
fn sic_bucket(upper: &str) -> &'static str {
    if upper.contains("AIRCRAFT") || upper.contains("MISSILE") || upper.contains("AERONAUT") {
        "space"
    } else {
        "biotech"
    }
}

/// Jurisdictions over-represented in US small-cap pump & dump schemes. Not a
/// judgement on the companies themselves: it is a base-rate observed on this
/// specific setup (a sub-$20 micro-float ripping in the premarket on a press
/// release), and it is applied as a score penalty, never as a hard reject — a
/// genuinely strong catalyst still clears the bar.
///
/// Keys are normalised (see `normalize_country`) because the source is free-text
/// SEC business-location wording.
const RISK_DOMICILES: &[&str] = &[
    "china",
    "peoples republic of china",
    "hong kong",
    "cayman islands",
    "british virgin islands",
    "bvi",
];

/// Lower-case, strip `.` `,` and apostrophes, collapse whitespace. Mirrors
/// `normalize()` in src/lib/countries.ts so "U.S.A", "USA" and "United States"
/// agree on both sides; the apostrophe is stripped on top of the frontend's rule
/// because "People's Republic of China" is a label the SEC actually returns.
fn normalize_country(raw: &str) -> String {
    raw.to_lowercase()
        .replace(['.', ',', '\'', '\u{2019}'], "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// What the A+ gates need to know about the issuer behind a ticker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IssuerProfile {
    /// The industry pulls a premarket crowd (see `HOT_SIC_TITLES`/`HOT_KEYWORDS`).
    pub sector_hot: bool,
    /// Which crowd, when `sector_hot`: biotech / space / crypto / quantum /
    /// cannabis / ia. None when the industry is unknown or not hot.
    pub sector_bucket: Option<&'static str>,
    /// The issuer is domiciled in a jurisdiction with a high pump & dump base rate.
    pub country_risk: bool,
}

impl IssuerProfile {
    /// Build the profile from whatever `company_meta` holds. Every field is
    /// optional because the metadata pipeline fills them asynchronously — an
    /// unknown industry is simply "not hot", never a rejection.
    pub fn from_meta(country: Option<&str>, industry: Option<&str>, sector: Option<&str>) -> Self {
        let (sector_hot, sector_bucket) = hot_industry(industry, sector);
        Self {
            sector_hot,
            sector_bucket,
            country_risk: is_risk_domicile(country),
        }
    }
}

/// Whether an industry/sector label is a "hot" category, and which one. `industry`
/// is checked first (it carries the SIC title); `sector` is the broader fallback.
pub fn hot_industry(industry: Option<&str>, sector: Option<&str>) -> (bool, Option<&'static str>) {
    for label in [industry, sector].into_iter().flatten() {
        let upper = label.trim().to_uppercase();
        if upper.is_empty() {
            continue;
        }
        if HOT_SIC_TITLES.contains(&upper.as_str()) {
            return (true, Some(sic_bucket(&upper)));
        }
        let lower = upper.to_lowercase();
        if let Some((_, bucket)) = HOT_KEYWORDS.iter().find(|(kw, _)| lower.contains(kw)) {
            return (true, Some(bucket));
        }
    }
    (false, None)
}

/// Whether a free-text country label names a high-risk domicile.
pub fn is_risk_domicile(country: Option<&str>) -> bool {
    match country {
        Some(c) if !c.trim().is_empty() => {
            let n = normalize_country(c);
            RISK_DOMICILES.iter().any(|d| n == *d)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn official_sic_titles_are_hot_including_secs_own_typos() {
        // The exact-title path exists because substring keywords miss these.
        assert_eq!(
            hot_industry(
                Some("BIOLOGICAL PRODUCTS, (NO DISGNOSTIC SUBSTANCES)"),
                None
            ),
            (true, Some("biotech"))
        );
        assert_eq!(
            hot_industry(
                Some("SEARCH, DETECTION, NAVAGATION, GUIDANCE, AERONAUTICAL SYS"),
                None
            ),
            (true, Some("space"))
        );
        assert_eq!(
            hot_industry(Some("GUIDED MISSILES & SPACE VEHICLES & PARTS"), None),
            (true, Some("space"))
        );
    }

    #[test]
    fn free_text_keywords_cover_what_sic_has_no_code_for() {
        for (label, bucket) in [
            ("Cannabis Producers", "cannabis"),
            ("Blockchain & Digital Assets", "crypto"),
            ("Quantum Computing", "quantum"),
            ("Commercial Space", "space"),
        ] {
            assert_eq!(
                hot_industry(Some(label), None),
                (true, Some(bucket)),
                "{label}"
            );
        }
    }

    #[test]
    fn sector_is_only_the_fallback_and_ordinary_industries_are_not_hot() {
        assert_eq!(
            hot_industry(None, Some("Healthcare Biotechnology")),
            (true, Some("biotech"))
        );
        assert_eq!(
            hot_industry(Some("RETAIL-EATING PLACES"), None),
            (false, None)
        );
        assert_eq!(hot_industry(None, None), (false, None));
        assert_eq!(hot_industry(Some("   "), None), (false, None));
    }

    #[test]
    fn risk_domiciles_survive_secs_free_text_punctuation() {
        for c in [
            "China",
            "china",
            "Hong Kong",
            "Cayman Islands",
            "  BVI ",
            "B.V.I.",
            "People's Republic of China",
            "Peoples Republic of China",
        ] {
            assert!(is_risk_domicile(Some(c)), "{c} should be flagged");
        }
        for c in ["U.S.A", "United States", "Israel", "Canada", ""] {
            assert!(!is_risk_domicile(Some(c)), "{c} should not be flagged");
        }
        assert!(!is_risk_domicile(None));
    }

    #[test]
    fn a_substring_match_never_flags_a_country() {
        // "Chinatown"/"Indochina" must not read as China — the domicile test is an
        // equality on the normalised label, not a `contains`.
        assert!(!is_risk_domicile(Some("Indochina")));
        assert!(!is_risk_domicile(Some("Republic of China (Taiwan)")));
    }

    #[test]
    fn profile_combines_both_reads() {
        let p = IssuerProfile::from_meta(
            Some("Cayman Islands"),
            Some("PHARMACEUTICAL PREPARATIONS"),
            None,
        );
        assert!(p.sector_hot && p.country_risk);
        assert_eq!(p.sector_bucket, Some("biotech"));
    }
}
