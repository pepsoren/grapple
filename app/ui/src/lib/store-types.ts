// The store contract (docs/STORE_PLAN.md, "Shared contract"). Mirrors the Rust side; do not change without the plan.
export type StoreKind = 'steam' | 'epic'
export type StoreKey = string // "steam:1621690" | "epic:<namespace>:<offerId>"
/** Always in the display currency (settings); the UI never converts. */
export interface Money { minor: number; currency: string }
export interface StorePrice {
  store: StoreKind; original: Money; final: Money; discountPct: number
  native: { minor: number; currency: string }
  /** Where Buy goes. */
  url: string
}
/** grapple://store/... URLs. */
export interface StoreArt { cover?: string; header?: string; hero?: string; logo?: string; shots: string[] }
export interface Similar { kind: 'game' | 'friend'; id: string; icon?: string; name: string }
export interface StoreGame {
  key: StoreKey; name: string; art: StoreArt; genres: string[]; tags: string[]
  short: string; developers: string[]; release?: string; reviews?: number; free: boolean; comingSoon: boolean
  prices: StorePrice[]; cheapest?: StoreKind; owned: boolean; wishlisted: boolean; notifyOnSale: boolean
  similar: Similar[]; lowestSeen?: Money
}
export interface StoreFront {
  featured: StoreGame[]; picked: StoreGame[]; top: StoreGame[]; deals: StoreGame[]
  under: { tierMinor: number; games: StoreGame[]; hidden: number }
  newReleases: { games: StoreGame[]; hidden: number }
  wishlist: StoreGame[]; epicFree: { game: StoreGame; now: boolean; until?: string }[]
  genres: { name: string; art: string }[]; updatedAt: number
}
export interface StoreSettings {
  displayCurrency: string; hideAdult: boolean; hideViolent: boolean
  stores: { steam: boolean; epic: boolean }; wishlistAlerts: boolean
  /** Set or not, never the key. */
  itadKey?: boolean
}
