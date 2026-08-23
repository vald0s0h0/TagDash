// Country → flag emoji + English/French display name, for the chart top bar's
// country chip. Keyed by the free-text label sec-api.io returns (SEC EDGAR
// business-location convention, e.g. "U.S.A" not "United States", see
// src-tauri/src/sec_api/mod.rs) — looked up through `normalize` so minor
// spelling/punctuation variants ("USA", "U.S.A.", "United States") still match.

export interface CountryMeta {
  flag: string;
  en: string;
  fr: string;
}

const COUNTRIES: Record<string, CountryMeta> = {
  usa: { flag: "🇺🇸", en: "United States", fr: "États-Unis" },
  us: { flag: "🇺🇸", en: "United States", fr: "États-Unis" },
  "united states": { flag: "🇺🇸", en: "United States", fr: "États-Unis" },
  "united states of america": { flag: "🇺🇸", en: "United States", fr: "États-Unis" },
  china: { flag: "🇨🇳", en: "China", fr: "Chine" },
  "hong kong": { flag: "🇭🇰", en: "Hong Kong", fr: "Hong Kong" },
  macau: { flag: "🇲🇴", en: "Macau", fr: "Macao" },
  taiwan: { flag: "🇹🇼", en: "Taiwan", fr: "Taïwan" },
  canada: { flag: "🇨🇦", en: "Canada", fr: "Canada" },
  "cayman islands": { flag: "🇰🇾", en: "Cayman Islands", fr: "Îles Caïmans" },
  "british virgin islands": { flag: "🇻🇬", en: "British Virgin Islands", fr: "Îles Vierges britanniques" },
  bvi: { flag: "🇻🇬", en: "British Virgin Islands", fr: "Îles Vierges britanniques" },
  bermuda: { flag: "🇧🇲", en: "Bermuda", fr: "Bermudes" },
  "marshall islands": { flag: "🇲🇭", en: "Marshall Islands", fr: "Îles Marshall" },
  israel: { flag: "🇮🇱", en: "Israel", fr: "Israël" },
  "united kingdom": { flag: "🇬🇧", en: "United Kingdom", fr: "Royaume-Uni" },
  uk: { flag: "🇬🇧", en: "United Kingdom", fr: "Royaume-Uni" },
  "great britain": { flag: "🇬🇧", en: "United Kingdom", fr: "Royaume-Uni" },
  ireland: { flag: "🇮🇪", en: "Ireland", fr: "Irlande" },
  germany: { flag: "🇩🇪", en: "Germany", fr: "Allemagne" },
  france: { flag: "🇫🇷", en: "France", fr: "France" },
  netherlands: { flag: "🇳🇱", en: "Netherlands", fr: "Pays-Bas" },
  luxembourg: { flag: "🇱🇺", en: "Luxembourg", fr: "Luxembourg" },
  switzerland: { flag: "🇨🇭", en: "Switzerland", fr: "Suisse" },
  belgium: { flag: "🇧🇪", en: "Belgium", fr: "Belgique" },
  sweden: { flag: "🇸🇪", en: "Sweden", fr: "Suède" },
  norway: { flag: "🇳🇴", en: "Norway", fr: "Norvège" },
  denmark: { flag: "🇩🇰", en: "Denmark", fr: "Danemark" },
  finland: { flag: "🇫🇮", en: "Finland", fr: "Finlande" },
  iceland: { flag: "🇮🇸", en: "Iceland", fr: "Islande" },
  italy: { flag: "🇮🇹", en: "Italy", fr: "Italie" },
  spain: { flag: "🇪🇸", en: "Spain", fr: "Espagne" },
  portugal: { flag: "🇵🇹", en: "Portugal", fr: "Portugal" },
  greece: { flag: "🇬🇷", en: "Greece", fr: "Grèce" },
  austria: { flag: "🇦🇹", en: "Austria", fr: "Autriche" },
  poland: { flag: "🇵🇱", en: "Poland", fr: "Pologne" },
  cyprus: { flag: "🇨🇾", en: "Cyprus", fr: "Chypre" },
  malta: { flag: "🇲🇹", en: "Malta", fr: "Malte" },
  monaco: { flag: "🇲🇨", en: "Monaco", fr: "Monaco" },
  gibraltar: { flag: "🇬🇮", en: "Gibraltar", fr: "Gibraltar" },
  jersey: { flag: "🇯🇪", en: "Jersey", fr: "Jersey" },
  guernsey: { flag: "🇬🇬", en: "Guernsey", fr: "Guernesey" },
  "isle of man": { flag: "🇮🇲", en: "Isle of Man", fr: "Île de Man" },
  australia: { flag: "🇦🇺", en: "Australia", fr: "Australie" },
  "new zealand": { flag: "🇳🇿", en: "New Zealand", fr: "Nouvelle-Zélande" },
  japan: { flag: "🇯🇵", en: "Japan", fr: "Japon" },
  "south korea": { flag: "🇰🇷", en: "South Korea", fr: "Corée du Sud" },
  korea: { flag: "🇰🇷", en: "South Korea", fr: "Corée du Sud" },
  singapore: { flag: "🇸🇬", en: "Singapore", fr: "Singapour" },
  india: { flag: "🇮🇳", en: "India", fr: "Inde" },
  indonesia: { flag: "🇮🇩", en: "Indonesia", fr: "Indonésie" },
  malaysia: { flag: "🇲🇾", en: "Malaysia", fr: "Malaisie" },
  philippines: { flag: "🇵🇭", en: "Philippines", fr: "Philippines" },
  thailand: { flag: "🇹🇭", en: "Thailand", fr: "Thaïlande" },
  vietnam: { flag: "🇻🇳", en: "Vietnam", fr: "Vietnam" },
  "united arab emirates": { flag: "🇦🇪", en: "United Arab Emirates", fr: "Émirats arabes unis" },
  uae: { flag: "🇦🇪", en: "United Arab Emirates", fr: "Émirats arabes unis" },
  "saudi arabia": { flag: "🇸🇦", en: "Saudi Arabia", fr: "Arabie saoudite" },
  qatar: { flag: "🇶🇦", en: "Qatar", fr: "Qatar" },
  turkey: { flag: "🇹🇷", en: "Turkey", fr: "Turquie" },
  russia: { flag: "🇷🇺", en: "Russia", fr: "Russie" },
  ukraine: { flag: "🇺🇦", en: "Ukraine", fr: "Ukraine" },
  "south africa": { flag: "🇿🇦", en: "South Africa", fr: "Afrique du Sud" },
  nigeria: { flag: "🇳🇬", en: "Nigeria", fr: "Nigeria" },
  kenya: { flag: "🇰🇪", en: "Kenya", fr: "Kenya" },
  egypt: { flag: "🇪🇬", en: "Egypt", fr: "Égypte" },
  brazil: { flag: "🇧🇷", en: "Brazil", fr: "Brésil" },
  mexico: { flag: "🇲🇽", en: "Mexico", fr: "Mexique" },
  colombia: { flag: "🇨🇴", en: "Colombia", fr: "Colombie" },
  argentina: { flag: "🇦🇷", en: "Argentina", fr: "Argentine" },
  chile: { flag: "🇨🇱", en: "Chile", fr: "Chili" },
  peru: { flag: "🇵🇪", en: "Peru", fr: "Pérou" },
  panama: { flag: "🇵🇦", en: "Panama", fr: "Panama" },
  "puerto rico": { flag: "🇵🇷", en: "Puerto Rico", fr: "Porto Rico" },
};

function normalize(raw: string): string {
  return raw.toLowerCase().replace(/[.,]/g, "").trim();
}

/** Flag + EN/FR label for a country string as stored in CardInfo/AlertEnrichment
 *  (`country`). Unknown/unmapped labels still resolve — with a neutral globe
 *  flag and the raw text standing in for both languages — so a ticker with
 *  metadata never silently loses its country chip. */
export function getCountryMeta(raw: string | null | undefined): CountryMeta | null {
  const trimmed = raw?.trim();
  if (!trimmed) return null;
  return COUNTRIES[normalize(trimmed)] ?? { flag: "🌐", en: trimmed, fr: trimmed };
}
