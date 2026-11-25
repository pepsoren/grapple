// The store commands (docs/STORE_PLAN.md). In a plain browser these are answered by store-mock.ts (dev only).
import { call } from './ipc'
import type { StoreFront, StoreGame, StoreKey, StoreKind, StoreSettings } from './store-types'

/** A game page: the game plus the picks "More like this" shows. */
export type StoreGameDetail = StoreGame & { moreLikeThis?: StoreGame[] }

/** Mirrors the Rust `StoreFilters`; everything optional. */
export interface StoreFilters {
  genre?: string
  tag?: string
  /** Display-currency minor units; 0 = free games only. */
  maxPriceMinor?: number
  onSale?: boolean
  /** Restrict to one store's games. */
  store?: StoreKind
  /** Show games the adult/violent filters would hide. */
  includeHidden?: boolean
}

export interface Currency { code: string; name: string; /** Digits after the point (JPY 0, ZAR 2). */ decimals: number }

export interface UnderTier { tierMinor: number; games: StoreGame[]; hidden: number }

/** "Popular new releases": the same shape as the front's. */
export interface NewReleases { games: StoreGame[]; hidden: number }

/** Where Steam prices come from (`store_region_get`). `source` is `setting` once picked by hand. */
export interface StoreRegion { country: string; currency: string; source: string }

/** A Steam-wishlist game removed from grapple's list. */
export interface HiddenWish { key: StoreKey; name: string }

/** Currency lists may come as plain codes or {code, name}. */
function currencies(raw: unknown): Currency[] {
  return (Array.isArray(raw) ? raw : []).map((c) => (typeof c === 'string' ? { code: c, name: c, decimals: 2 } : { ...(c as Currency), decimals: (c as Currency).decimals ?? 2 }))
}

export const storeApi = {
  front: () => call<StoreFront>('store_front'),
  game: (key: StoreKey) => call<StoreGameDetail>('store_game', { key }),
  under: (tierMinor: number, includeHidden = false) => call<UnderTier>('store_under', { tierMinor, includeHidden }),
  newReleases: (includeHidden = false) => call<NewReleases>('store_new_releases', { includeHidden }),
  search: (q: string, filters: StoreFilters = {}) => call<StoreGame[]>('store_search', { q, filters }),
  wishlist: () => call<StoreGame[]>('store_wishlist'),
  wishlistSet: (key: StoreKey, on: boolean) => call<void>('store_wishlist_set', { key, on }),
  wishlistHidden: () => call<HiddenWish[]>('store_wishlist_hidden'),
  /** Brings every hidden Steam-wishlist game back; answers how many. */
  wishlistUnhide: () => call<number>('store_wishlist_unhide'),
  notifySet: (key: StoreKey, on: boolean) => call<void>('store_notify_set', { key, on }),
  homePicks: () => call<StoreGame[]>('store_home_picks'),
  settingsGet: () => call<StoreSettings>('store_settings_get'),
  settingsSet: (settings: StoreSettings) => call<StoreSettings>('store_settings_set', { settings }),
  regionGet: () => call<StoreRegion>('store_region_get'),
  /** A two-letter country code, or null for automatic. */
  regionSet: (country: string | null) => call<StoreRegion>('store_region_set', { country }),
  /** Sets (or, with null, removes) the IsThereAnyDeal key; answers the settings, never the key. */
  itadKeySet: (key: string | null) => call<StoreSettings>('store_itad_key_set', { key }),
  currencies: async () => currencies(await call<unknown>('store_currencies')),
  /** Opens the store window at that store's checkout. */
  checkout: (key: StoreKey, store: StoreKind) => call<void>('store_checkout', { key, store }),
}
