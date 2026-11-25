// Countries for the Steam region setting: ISO 3166-1 codes, named by the browser (English), current one first.
const CODES = (
  'AD AE AF AG AI AL AM AO AR AT AU AW AZ BA BB BD BE BF BG BH BI BJ BM BN BO BR BS BT BW BY BZ CA CD CF CG CH CI CL CM CN CO CR CU CV CY CZ ' +
  'DE DJ DK DM DO DZ EC EE EG ER ES ET FI FJ FM FO FR GA GB GD GE GH GI GL GM GN GQ GR GT GU GW GY HK HN HR HT HU ID IE IL IN IQ IR IS IT JM JO JP ' +
  'KE KG KH KI KM KN KR KW KY KZ LA LB LC LI LK LR LS LT LU LV LY MA MC MD ME MG MK ML MM MN MO MR MT MU MV MW MX MY MZ NA NC NE NG NI NL NO NP NR NZ ' +
  'OM PA PE PF PG PH PK PL PR PS PT PW PY QA RO RS RU RW SA SB SC SD SE SG SI SK SL SM SN SO SR SS ST SV SY SZ TC TD TG TH TJ TL TM TN TO TR TT TV TW TZ ' +
  'UA UG US UY UZ VA VC VE VG VI VN VU WS XK YE ZA ZM ZW'
).split(' ')

let names: Intl.DisplayNames | null = null
try { names = new Intl.DisplayNames(['en'], { type: 'region' }) } catch { /* codes stand in for names */ }

export const countryName = (code: string) => {
  try { return names?.of(code) ?? code } catch { return code }
}

/** Every country by name, with `current` first. */
export function countryList(current: string): { code: string; name: string }[] {
  const all = [...new Set([...CODES, current])].filter(Boolean).map((code) => ({ code, name: countryName(code) }))
  all.sort((a, b) => a.name.localeCompare(b.name))
  const i = all.findIndex((c) => c.code === current)
  return i > 0 ? [all[i], ...all.slice(0, i), ...all.slice(i + 1)] : all
}
