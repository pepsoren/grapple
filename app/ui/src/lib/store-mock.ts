// Browser-only stand-in for the store commands (vite dev, screenshots). Built from the real data the mockups used
// (store-mock-data.json: Steam prices in rand, lists, wishlist, Epic's free games; Epic prices and friends are
// samples, as in the mockups). Only devbackend.ts imports this, under import.meta.env.DEV, so it never ships.
import data from './store-mock-data.json'
import type { Currency } from './store-api'
import type { Money, Similar, StoreFront, StoreGame, StoreKind, StorePrice, StoreSettings } from './store-types'

interface App {
  n: string; g: string[]; s: string; d: string[]; r: string; v: number; f: boolean
  p: [number, number, number] | null; cd: number[]; a: { c: boolean; r: boolean; h: boolean; sh: number }
}
const apps = data.apps as unknown as Record<string, App>
const lists = data.lists as { top: string[]; new: string[]; under: string[] }
const owned = data.owned as Record<string, string>

const RATE_USD = 16.465 // rand per dollar, the sample Epic prices
const RATES: Record<string, number> = { ZAR: 1, USD: 1 / RATE_USD, EUR: 0.0525, GBP: 0.0455, AUD: 0.0925, CAD: 0.0835 }
const CURRENCIES: Currency[] = [
  { code: 'ZAR', name: 'South African rand', decimals: 2 }, { code: 'USD', name: 'US dollar', decimals: 2 }, { code: 'EUR', name: 'Euro', decimals: 2 },
  { code: 'GBP', name: 'British pound', decimals: 2 }, { code: 'AUD', name: 'Australian dollar', decimals: 2 }, { code: 'CAD', name: 'Canadian dollar', decimals: 2 },
]
// Epic samples (orig, final) in USD cents.
const EPIC: Record<string, [number, number]> = { '1091500': [5999, 1799], '1174180': [5999, 5999], '990080': [5999, 599], '548430': [2999, 2999], '2807960': [6999, 2799] }
const ADULT_TAGGED = new Set(['4771580'])
const SIM: Record<string, string[]> = {
  '1172710': ['252490', '2399830'], '1621690': ['1169040', '892970'], '2646460': ['2399830', '1604030'], '1966720': ['3241660', '2881650', '3527290'],
  '553850': ['730', '1144200'], '602960': ['1144200', '739630'], '2254990': ['242760', '648800', '252490'], '4159680': ['3527290', '3241660'],
}
const FRIENDS: Record<string, string[]> = { '548430': ['rowan', 'kai'] }

let settings: StoreSettings = { displayCurrency: 'USD', hideAdult: true, hideViolent: false, stores: { steam: true, epic: true }, wishlistAlerts: true, itadKey: false }
const wish = new Set<string>(data.wishlist as string[])
// The mock's Steam wishlist: removing one only hides it here, as the real store does.
const wishHidden = new Set<string>()
let region = { country: 'DE', currency: 'EUR', source: 'steam-log' }
let itadKey = false
const quiet = new Set<string>()

const rate = () => RATES[settings.displayCurrency] ?? 1
const zar = (minor: number): Money => ({ minor: Math.round(minor * rate()), currency: settings.displayCurrency })
const usd = (cents: number): Money => zar(cents * RATE_USD)

const art = (id: string, kind: string) => `/@dev-store/${id}-${kind}.jpg`
const adult = (id: string) => ADULT_TAGGED.has(id) || apps[id].cd.some((c) => [1, 3, 4].includes(c))
const violent = (id: string) => apps[id].cd.includes(2)
const hidden = (id: string) => (settings.hideAdult && adult(id)) || (settings.hideViolent && violent(id))

function similar(id: string): Similar[] {
  const g: Similar[] = (SIM[id] ?? []).filter((x) => owned[x]).map((x) => ({
    kind: 'game', id: `steam:${x}`, name: owned[x], icon: `/@dev-art/${encodeURIComponent(`steam:${x}`)}/icon`,
  }))
  const f: Similar[] = (FRIENDS[id] ?? []).map((n) => ({ kind: 'friend', id: `tether:${n}`, name: n }))
  return [...g, ...f]
}

function prices(id: string): StorePrice[] {
  const a = apps[id]
  const out: StorePrice[] = []
  if (a.p) {
    out.push({
      store: 'steam', original: zar(a.p[0]), final: zar(a.p[1]), discountPct: a.p[2],
      native: { minor: a.p[1], currency: 'ZAR' }, url: `https://store.steampowered.com/app/${id}/`,
    })
  }
  const e = EPIC[id]
  if (e) {
    out.push({
      store: 'epic', original: usd(e[0]), final: usd(e[1]), discountPct: e[0] ? Math.round(100 - (100 * e[1]) / e[0]) : 0,
      native: { minor: e[1], currency: 'USD' }, url: '',
    })
  }
  return out
}

function game(id: string): StoreGame {
  const a = apps[id]
  const ps = prices(id)
  const soon = a.r === 'Coming soon' || (!a.p && !a.f)
  const cheapest = ps.length > 1 ? ps.reduce((x, y) => (y.final.minor < x.final.minor ? y : x)).store : undefined
  const key = `steam:${id}`
  const steam = ps.find((p) => p.store === 'steam')
  return {
    key, name: a.n,
    art: {
      cover: a.a.c ? art(id, 'cover') : undefined, header: a.a.h ? art(id, 'header') : undefined, hero: a.a.r ? art(id, 'hero') : undefined,
      shots: Array.from({ length: a.a.sh }, (_, k) => art(id, `shot${k}`)),
    },
    genres: a.g, tags: a.g, short: a.s, developers: a.d, release: soon ? undefined : a.r || undefined, reviews: a.v || undefined,
    free: a.f, comingSoon: soon, prices: ps, cheapest, owned: false, wishlisted: wish.has(id) && !wishHidden.has(id), notifyOnSale: wish.has(id) && !wishHidden.has(id) && !quiet.has(id),
    similar: similar(id), lowestSeen: steam ? { ...steam.final } : undefined,
  }
}

function epicGame(f: { t: string; now: boolean; slug: string; usd: number }): StoreGame {
  const m = usd(f.usd)
  return {
    key: `epic:mock:${f.slug}`, name: f.t, art: { cover: art('epic', f.slug), shots: [] }, genres: [], tags: [], short: '', developers: [],
    free: f.now, comingSoon: false,
    prices: [{ store: 'epic', original: m, final: { ...m, minor: 0 }, discountPct: 100, native: { minor: 0, currency: 'USD' }, url: '' }],
    owned: false, wishlisted: false, notifyOnSale: false, similar: [],
  }
}

const shown = (ids: string[]) => ids.filter((i) => apps[i] && !hidden(i)).map(game)
const NEWPICK = ['2347080', '3470960', '2254990', '4159680', '3115160', '2828710', '4231820', '3826670']
const WISH_STRIP = ['1245620', '221100', '1928980', '951440', '2253100', '758690', '2178070', '2643010', '2332260', '2721340']
const GENRES: [string, string][] = [['Survival', '2646460'], ['Co-op', '548430'], ['Shooter', '553850'], ['RPG', '1091500'], ['Simulation', '3219030'], ['Strategy', '2347080']]

function newReleases(includeHidden = false) {
  const all = [...NEWPICK.slice(0, 3), ...lists.new.filter((i) => apps[i] && adult(i)), ...NEWPICK.slice(3)].filter((i) => apps[i])
  const hiddenN = all.filter((i) => hidden(i)).length
  return { games: all.filter((i) => includeHidden || !hidden(i)).slice(0, 8).map(game), hidden: includeHidden ? 0 : hiddenN }
}

function under(tierMinor: number, includeHidden = false) {
  // The tier is in display-currency minor units, like the prices.
  const order = (i: string) => { const k = lists.under.indexOf(i); return k < 0 ? 999 : k }
  const ids = Object.keys(apps)
    .filter((i) => apps[i].p && !apps[i].f && apps[i].p![1] > 0 && zar(apps[i].p![1]).minor <= tierMinor)
    .sort((x, y) => order(x) - order(y) || apps[y].p![2] - apps[x].p![2] || apps[x].p![1] - apps[y].p![1])
  return { tierMinor, games: ids.filter((i) => includeHidden || !hidden(i)).slice(0, 12).map(game), hidden: includeHidden ? 0 : ids.filter(hidden).length }
}

function front(): StoreFront {
  const free = (data.epicFree as { t: string; now: boolean; slug: string; usd: number }[])
  const sel = [...free.filter((f) => f.now), ...free.filter((f) => !f.now).slice(0, 2)]
  return {
    featured: shown(['1172710', '1621690', '2646460', '1966720']),
    picked: shown(['1621690', '2646460', '1966720', '553850', '548430', '4159680']),
    top: shown(lists.top).slice(0, 10),
    deals: shown(['2807960', '1091500', '990080', '208650', '356190', '976310', '627270']).slice(0, 4),
    under: under(settings.displayCurrency === 'ZAR' ? 10000 : 1000),
    newReleases: newReleases(),
    wishlist: WISH_STRIP.filter((i) => wish.has(i) && !wishHidden.has(i)).map(game),
    epicFree: sel.map((f) => ({ game: epicGame(f), now: f.now })),
    genres: GENRES.map(([name, id]) => ({ name, art: art(id, apps[id].a.r ? 'hero' : 'header') })),
    updatedAt: Math.floor(Date.now() / 1000),
  }
}

const stores = (g: StoreGame) => g.prices.some((p) => settings.stores[p.store as StoreKind]) || !g.prices.length

function search(q: string, f: { genre?: string; maxPriceMinor?: number; includeHidden?: boolean }) {
  const needle = q.toLowerCase()
  const maxZar = f.maxPriceMinor == null ? null : f.maxPriceMinor / rate()
  return Object.keys(apps)
    .filter((i) => f.includeHidden || !hidden(i))
    .filter((i) => !needle || apps[i].n.toLowerCase().includes(needle) || apps[i].g.some((x) => x.toLowerCase().includes(needle)) || apps[i].d.some((x) => x.toLowerCase().includes(needle)))
    .filter((i) => !f.genre || apps[i].g.includes(f.genre) || apps[i].s.toLowerCase().includes(f.genre.toLowerCase()) || (f.genre === 'Co-op' && /co-?op|friends/i.test(apps[i].s)))
    .filter((i) => maxZar == null || (maxZar === 0 ? apps[i].f : !!apps[i].p && !apps[i].f && apps[i].p![1] <= maxZar))
    .map(game)
    .filter(stores)
    .slice(0, 60)
}

export async function storeInvoke(cmd: string, args: Record<string, unknown>): Promise<unknown> {
  switch (cmd) {
    case 'store_front': return front()
    case 'store_game': {
      const key = String(args.key)
      const id = key.split(':')[1]
      if (key.startsWith('epic:')) {
        const slug = key.split(':')[2]
        const f = (data.epicFree as { t: string; now: boolean; slug: string; usd: number }[]).find((x) => x.slug === slug)
        if (!f) throw 'Not found'
        return { ...epicGame(f), moreLikeThis: [] }
      }
      if (!apps[id]) throw 'Not found'
      return { ...game(id), moreLikeThis: shown(['2646460', '2254990', '4159680', '548430'].filter((x) => x !== id)) }
    }
    case 'store_under': return under(Number(args.tierMinor), args.includeHidden === true)
    case 'store_new_releases': return newReleases(args.includeHidden === true)
    case 'store_search': return search(String(args.q ?? ''), (args.filters ?? {}) as { genre?: string; maxPriceMinor?: number; includeHidden?: boolean })
    case 'store_wishlist': return [...wish].filter((i) => apps[i] && !wishHidden.has(i)).map(game)
    case 'store_wishlist_set': {
      const id = String(args.key).split(':')[1]
      if (args.on) { wish.add(id); wishHidden.delete(id); quiet.delete(id) } else wishHidden.add(id)
      return
    }
    case 'store_wishlist_hidden': return [...wishHidden].filter((i) => apps[i]).map((i) => ({ key: `steam:${i}`, name: apps[i].n }))
    case 'store_wishlist_unhide': { const n = wishHidden.size; wishHidden.clear(); return n }
    case 'store_region_get': return region
    case 'store_region_set': {
      const c = args.country as string | null
      region = c ? { country: c, currency: c === 'ZA' ? 'ZAR' : 'USD', source: 'setting' } : { country: 'DE', currency: 'EUR', source: 'steam-log' }
      return region
    }
    case 'store_itad_key_set': itadKey = typeof args.key === 'string' && args.key.length > 0; settings = { ...settings, itadKey }; return settings
    case 'store_notify_set': {
      const id = String(args.key).split(':')[1]
      if (args.on) quiet.delete(id); else quiet.add(id)
      return
    }
    case 'store_home_picks': return shown(['1621690', '1966720', '2646460', '4159680', '553850'])
    case 'store_settings_get': return settings
    case 'store_settings_set': settings = args.settings as StoreSettings; return settings
    case 'store_currencies': return CURRENCIES
    case 'store_checkout': console.info('[store mock] checkout', args); return
    default: throw `Unknown command ${cmd}`
  }
}
