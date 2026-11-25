// Display helpers for store prices. The backend already converted everything into the display currency;
// this only formats a Money value and picks which price to lead with.
import type { Money, StoreGame, StoreKind, StorePrice } from './store-types'

const SYMBOL: Record<string, string> = { ZAR: 'R', USD: '$', EUR: '€', GBP: '£', JPY: '¥', CNY: '¥', INR: '₹', KRW: '₩', BRL: 'R$', PLN: 'zł', TRY: '₺', UAH: '₴', RUB: '₽' }
/** Digits after the point per currency: the backend's currency list fills this in; ISO defaults until then. */
const DECIMALS: Record<string, number> = { JPY: 0, KRW: 0, VND: 0, CLP: 0, ISK: 0, BHD: 3, KWD: 3, OMR: 3, JOD: 3 }
export function setDecimals(list: { code: string; decimals: number }[]) {
  for (const c of list) DECIMALS[c.code] = c.decimals
}

export const currencySymbol = (code: string) => SYMBOL[code] ?? code

/** "R 1 199.00", "$19.99", "CHF 12.00": symbol, thin grouping spaces, a point decimal. */
export function money(m: Money): string {
  const sym = currencySymbol(m.currency)
  const dec = DECIMALS[m.currency] ?? 2
  const v = m.minor / 10 ** dec
  const [int, frac] = Math.abs(v).toFixed(dec).split('.')
  const grouped = int.replace(/\B(?=(\d{3})+(?!\d))/g, '\u00a0')
  const num = frac ? `${grouped}.${frac}` : grouped
  const sep = sym.length > 1 || /^[A-Z]$/.test(sym) ? '\u00a0' : ''
  return `${v < 0 ? '-' : ''}${sym}${sep}${num}`
}

/** "R · ZAR" for the currency pill. */
export const currencyLabel = (code: string) => `${currencySymbol(code)} · ${code}`

/** A whole-unit tier ("R 100") from a minor amount. */
export const tierLabel = (tierMinor: number, currency: string) => money({ minor: tierMinor, currency }).replace(/\.0+$/, '')

export const storeName = (s: StoreKind) => (s === 'steam' ? 'Steam' : 'Epic')

/** The price a card leads with: the cheapest of the stores that have one. */
export function leadPrice(g: StoreGame): StorePrice | undefined {
  if (!g.prices.length) return undefined
  if (g.cheapest) {
    const c = g.prices.find((p) => p.store === g.cheapest)
    if (c) return c
  }
  return g.prices.reduce((a, b) => (b.final.minor < a.final.minor ? b : a))
}

export const priceOn = (g: StoreGame, s: StoreKind) => g.prices.find((p) => p.store === s)

/** The biggest discount across the stores (0 when none). */
export const bestDiscount = (g: StoreGame) => g.prices.reduce((a, p) => Math.max(a, p.discountPct), 0)

/** "Cheaper on Steam" / "Cheaper on Epic" / "Same price on both" / null (only one store has a price). */
export function advantage(g: StoreGame): { text: string; better: boolean } | null {
  const s = priceOn(g, 'steam')
  const e = priceOn(g, 'epic')
  if (!s || !e) return null
  if (s.final.minor < e.final.minor * 0.97) return { text: 'Cheaper on Steam', better: true }
  if (e.final.minor < s.final.minor * 0.97) return { text: 'Cheaper on Epic', better: true }
  return { text: 'Same price on both', better: false }
}

const SMALL_UNIT = new Set(['USD', 'EUR', 'GBP', 'CAD', 'AUD', 'CHF', 'NZD'])
/** The "Under" price tiers in minor units: 5/10/20/30 of a dollar-sized currency, 50/100/200/300 of a rand-sized one. */
export const tiersFor = (currency: string) => (SMALL_UNIT.has(currency) ? [500, 1000, 2000, 3000] : [5000, 10000, 20000, 30000])
