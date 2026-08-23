// Industries that get a highlighted "hot" badge — categories known for outsized,
// news-driven premarket moves (biotech catalysts, crypto/quantum/space hype
// cycles, cannabis legalisation swings).
//
// `ScreenerMatch.industry` / `CardInfo.industry` are populated in
// src-tauri/src/sec_api/mod.rs from, in order: (1) the official SEC SIC code
// title (src-tauri/src/sec_api/sic_codes.rs, ALL CAPS, e.g.
// "PHARMACEUTICAL PREPARATIONS"), (2) sec-api's own `sicIndustry` field, or
// (3) sec-api's broader `industry` field — both free text, unknown casing.
//
// The 1938-vintage SIC schema has NO code at all for cannabis, crypto or
// quantum, so those can only ever surface via the free-text fallback. Biotech
// and space/aerospace DO have real SIC codes, so those are matched by an exact
// (manually curated) title list first — substring keywords alone miss them,
// since e.g. "PHARMACEUTICAL PREPARATIONS" contains no "biotech".

/** Exact SEC SIC titles (case-insensitive) known to belong to a hot category.
 *  Sourced by hand from sic_codes.rs — the codes are commented for traceability
 *  if that table is ever regenerated and a title's wording changes. */
const HOT_SIC_TITLES = new Set(
  [
    // Biotechnology / pharma / life-sciences research
    "MEDICINAL CHEMICALS & BOTANICAL PRODUCTS",              // 2833
    "PHARMACEUTICAL PREPARATIONS",                            // 2834
    "IN VITRO & IN VIVO DIAGNOSTIC SUBSTANCES",               // 2835
    "BIOLOGICAL PRODUCTS, (NO DISGNOSTIC SUBSTANCES)",        // 2836 (sic — SEC's own typo)
    "SERVICES-COMMERCIAL PHYSICAL & BIOLOGICAL RESEARCH",     // 8731
    // Space / aerospace
    "GUIDED MISSILES & SPACE VEHICLES & PARTS",               // 3760
    "SEARCH, DETECTION, NAVAGATION, GUIDANCE, AERONAUTICAL SYS", // 3812 (sic)
    "AIRCRAFT & PARTS",                                       // 3720
    "AIRCRAFT",                                                // 3721
    "AIRCRAFT ENGINES & ENGINE PARTS",                        // 3724
    "AIRCRAFT PARTS & AUXILIARY EQUIPMENT, NEC",              // 3728
  ].map((s) => s.toUpperCase()),
);

/** Free-text keyword fallback — the only path for categories with no SIC code
 *  (cannabis, crypto, quantum), and a safety net for biotech/space wording sec-
 *  api's own fields may use instead of the official SIC title. */
const HOT_INDUSTRY_KEYWORDS = [
  "biotech",
  "biopharma",
  "cannabis",
  "marijuana",
  "hemp",
  "crypto",
  "bitcoin",
  "blockchain",
  "quantum",
  "space",
  "aerospace",
];

export function isHotIndustry(industry: string | null | undefined): boolean {
  if (!industry) return false;
  const upper = industry.trim().toUpperCase();
  if (HOT_SIC_TITLES.has(upper)) return true;
  const lower = upper.toLowerCase();
  return HOT_INDUSTRY_KEYWORDS.some((kw) => lower.includes(kw));
}

/** Green outline badge — border + text green, transparent background — used for
 *  both the screener card and the chart top bar's industry chip. */
export const HOT_INDUSTRY_BADGE_CLASS =
  "border border-emerald-500/70 bg-transparent text-emerald-400";
