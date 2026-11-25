// Store state: what the pages render from. Everything shown comes from the backend's cache first
// (store_front etc. answer from SQLite), then `store-changed` refreshes it in the background.
import { listen } from './ipc'
import { createStore, useStore } from './store'
import { storeApi, type Currency, type HiddenWish, type NewReleases, type StoreGameDetail, type StoreRegion, type UnderTier } from './store-api'
import { setDecimals } from './store-format'
import { storeNav } from './store-nav'
import type { StoreFront, StoreGame, StoreKey, StoreKind, StoreSettings } from './store-types'
import { toast } from './ui'

interface StoreState {
  front: StoreFront | null
  /** The front failed to load (backend not ready, offline): the page says so once. */
  failed: boolean
  settings: StoreSettings | null
  /** The whole wishlist (the front only carries a strip of it). */
  wishlist: StoreGame[]
  currencies: Currency[]
  /** A price tier picked in "Under" (or its "Show"); null = the front's own. */
  under: UnderTier | null
  /** "Popular new releases" with its hidden games shown (its "Show"); null = the front's own. */
  newReleases: NewReleases | null
  /** Where Steam prices come from. */
  region: StoreRegion | null
  /** Steam-wishlist games removed from the wishlist here. */
  hiddenWishlist: HiddenWish[]
  details: Record<StoreKey, StoreGameDetail>
  homePicks: StoreGame[]
}

export const storeData = createStore<StoreState>({
  front: null, failed: false, settings: null, wishlist: [], currencies: [], under: null, newReleases: null, region: null, hiddenWishlist: [], details: {}, homePicks: [],
})
export const useStoreData = <S>(select: (s: StoreState) => S) => useStore(storeData, select)

const fail = (e: unknown) => toast(typeof e === 'string' ? e : 'Something went wrong')

async function loadFront() {
  try {
    const front = await storeApi.front()
    storeData.set({ front, failed: false })
  } catch {
    storeData.set({ failed: storeData.get().front == null })
  }
}

async function loadWishlist() {
  try {
    storeData.set({ wishlist: await storeApi.wishlist() })
  } catch { /* keeps the last list */ }
  void loadHiddenWishlist()
}

async function loadHiddenWishlist() {
  try {
    storeData.set({ hiddenWishlist: await storeApi.wishlistHidden() })
  } catch { /* keeps the last list */ }
}

let watching = false
/** One `store-changed` listener for the whole app: the Home picks always follow it, the store pages once opened. */
function watch() {
  if (watching) return
  watching = true
  // The backend raises this about once a second while a refresh runs: refetch once it settles.
  let t = 0
  void listen<unknown>('store-changed', () => {
    clearTimeout(t)
    t = window.setTimeout(() => void refreshAll(false), 600)
  })
}

/** Home's "Games we think you'd like". */
export async function loadHomePicks() {
  watch()
  try {
    storeData.set({ homePicks: await storeApi.homePicks() })
  } catch {
    storeData.set({ homePicks: [] })
  }
}

/** Just the settings (the avatar menu's Store section); the pages load the rest when opened. */
export async function loadStoreSettings() {
  if (storeData.get().settings) return
  try {
    storeData.set({ settings: await storeApi.settingsGet() })
  } catch { /* the section stays hidden */ }
}

let started = false
/** First visit (or the Home picks): load what the store pages need and follow `store-changed`. */
export function startStore() {
  if (started) return
  started = true
  void loadFront()
  void loadWishlist()
  void storeApi.settingsGet().then((settings) => storeData.set({ settings })).catch(() => {})
  void storeApi.regionGet().then((region) => storeData.set({ region })).catch(() => {})
  void storeApi.currencies().then((currencies) => { setDecimals(currencies); storeData.set({ currencies }) }).catch(() => {})
  watch()
}

/** Everything that carries prices or filters again (a setting changed, or a section refreshed). The open game
 *  page keeps showing its old data until the new arrives. */
async function refreshAll(settingsChanged: boolean) {
  const page = storeNav.get().page
  const open = page.kind === 'game' ? page.key : null
  const cur = storeData.get()
  const tier = settingsChanged ? null : cur.under
  const news = settingsChanged ? null : cur.newReleases
  storeData.set({ details: open && cur.details[open] ? { [open]: cur.details[open] } : {}, under: tier, newReleases: news })
  await Promise.all([
    started ? loadFront() : null,
    started ? loadWishlist() : null,
    loadHomePicks(),
    open ? storeApi.game(open).then((d) => storeData.set((s) => ({ details: { ...s.details, [open]: d } }))).catch(() => {}) : null,
    tier ? storeApi.under(tier.tierMinor, underShown).then((u) => storeData.set({ under: u })).catch(() => {}) : null,
    news ? storeApi.newReleases(true).then((n) => storeData.set({ newReleases: n })).catch(() => {}) : null,
  ])
}

export async function loadGame(key: StoreKey): Promise<StoreGameDetail | null> {
  const have = storeData.get().details[key]
  if (have) return have
  try {
    const d = await storeApi.game(key)
    storeData.set((s) => ({ details: { ...s.details, [key]: d } }))
    return d
  } catch {
    return null
  }
}

/** The "Under" row for a tier; `includeHidden` is the row's "Show" (this row only, until the next refresh). */
let underShown = false
export async function loadTier(tierMinor: number, includeHidden = false) {
  underShown = includeHidden
  try {
    storeData.set({ under: await storeApi.under(tierMinor, includeHidden) })
  } catch (e) {
    fail(e)
  }
}

/** "Popular new releases" with the games the filters hid (its "Show": this row only, until the next settings change). */
export async function showHiddenNew() {
  try {
    storeData.set({ newReleases: await storeApi.newReleases(true) })
  } catch (e) {
    fail(e)
  }
}

/** Patch one game wherever it is cached. */
function patchGame(key: StoreKey, patch: Partial<StoreGame>) {
  const p = (g: StoreGame) => (g.key === key ? { ...g, ...patch } : g)
  const list = (a: StoreGame[]) => (a.some((g) => g.key === key) ? a.map(p) : a)
  storeData.set((s) => {
    const f = s.front
    return {
      front: f && {
        ...f, featured: list(f.featured), picked: list(f.picked), top: list(f.top), deals: list(f.deals), wishlist: list(f.wishlist),
        under: { ...f.under, games: list(f.under.games) }, newReleases: { ...f.newReleases, games: list(f.newReleases.games) },
        epicFree: f.epicFree.map((x) => (x.game.key === key ? { ...x, game: p(x.game) } : x)),
      },
      wishlist: list(s.wishlist),
      homePicks: list(s.homePicks),
      under: s.under && { ...s.under, games: list(s.under.games) },
      newReleases: s.newReleases && { ...s.newReleases, games: list(s.newReleases.games) },
      details: s.details[key] ? { ...s.details, [key]: { ...s.details[key], ...patch } } : s.details,
    }
  })
}

export async function toggleWishlist(g: Pick<StoreGame, 'key' | 'wishlisted'>) {
  const on = !g.wishlisted
  patchGame(g.key, { wishlisted: on, notifyOnSale: on ? storeData.get().settings?.wishlistAlerts ?? true : false })
  try {
    await storeApi.wishlistSet(g.key, on)
    await loadWishlist()
  } catch (e) {
    patchGame(g.key, { wishlisted: !on })
    fail(e)
  }
}

/** Takes a game off the wishlist page. A Steam-wishlist game is only hidden here (grapple never changes the Steam
 *  wishlist); Settings can show those again. */
export async function removeFromWishlist(key: StoreKey) {
  const before = storeData.get().wishlist
  storeData.set({ wishlist: before.filter((g) => g.key !== key) })
  patchGame(key, { wishlisted: false, notifyOnSale: false })
  try {
    await storeApi.wishlistSet(key, false)
    await loadWishlist()
    const f = storeData.get().front
    if (f) storeData.set({ front: { ...f, wishlist: f.wishlist.filter((g) => g.key !== key) } })
  } catch (e) {
    storeData.set({ wishlist: before })
    patchGame(key, { wishlisted: true })
    fail(e)
  }
}

/** Settings: bring back the Steam-wishlist games removed here. */
export async function showHiddenWishlist() {
  try {
    await storeApi.wishlistUnhide()
    storeData.set({ hiddenWishlist: [] })
    await Promise.all([loadWishlist(), loadFront()])
  } catch (e) {
    fail(e)
  }
}

/** Settings: the Steam store country ('' = automatic). Prices and lists follow it. */
export async function setStoreRegion(country: string | null) {
  try {
    storeData.set({ region: await storeApi.regionSet(country) })
    await refreshAll(true)
  } catch (e) {
    fail(e)
  }
}

/** Settings: save or remove the IsThereAnyDeal key. Only whether one is set comes back. */
export async function setItadKey(key: string | null): Promise<boolean> {
  try {
    storeData.set({ settings: await storeApi.itadKeySet(key) })
    return true
  } catch (e) {
    fail(e)
    return false
  }
}

export async function toggleNotify(g: Pick<StoreGame, 'key' | 'notifyOnSale'>) {
  const on = !g.notifyOnSale
  patchGame(g.key, { notifyOnSale: on })
  try {
    await storeApi.notifySet(g.key, on)
  } catch (e) {
    patchGame(g.key, { notifyOnSale: !on })
    fail(e)
  }
}

export async function updateStoreSettings(patch: Partial<StoreSettings>) {
  const cur = storeData.get().settings
  if (!cur) return
  const next = { ...cur, ...patch }
  storeData.set({ settings: next })
  try {
    storeData.set({ settings: await storeApi.settingsSet(next) })
    await refreshAll(true)
  } catch (e) {
    storeData.set({ settings: cur })
    fail(e)
  }
}

export async function buy(key: StoreKey, store: StoreKind) {
  try {
    await storeApi.checkout(key, store)
  } catch (e) {
    fail(e)
  }
}
