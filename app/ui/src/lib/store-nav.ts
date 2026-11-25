// Which store page is showing, and the way back. The route itself ('store') lives in ui.ts.
import { createStore, useStore } from './store'
import type { StoreFilters } from './store-api'
import type { StoreKey } from './store-types'
import { openSettings, uiStore } from './ui'

export type StorePage =
  | { kind: 'front' }
  | { kind: 'game'; key: StoreKey }
  | { kind: 'wishlist' }

interface Nav {
  page: StorePage
  /** Pages to go back to, with the scroll position they were left at. */
  stack: { page: StorePage; y: number }[]
  /** Where the page should scroll to when it appears (the saved position on a way back, else the top). */
  y: number
  /** Bumped on every change, so the page knows to scroll even when the page itself is unchanged. */
  tick: number
}

export const storeNav = createStore<Nav>({ page: { kind: 'front' }, stack: [], y: 0, tick: 0 })
export const useStoreNav = <S>(select: (s: Nav) => S) => useStore(storeNav, select)

let scroller: HTMLElement | null = null
export const registerStoreScroller = (el: HTMLElement | null) => { scroller = el }

function go(page: StorePage) {
  const cur = storeNav.get()
  uiStore.set({ route: 'store', menu: false, drawer: false, palette: false })
  storeNav.set({ page, stack: [...cur.stack, { page: cur.page, y: scroller?.scrollTop ?? 0 }], y: 0, tick: cur.tick + 1 })
}

/** The store front. Keeps the page you were on when you are already in the store. */
export function openStore() {
  uiStore.set({ route: 'store', menu: false, drawer: false, palette: false })
}

export function storeHome() {
  const cur = storeNav.get()
  uiStore.set({ route: 'store', menu: false })
  storeNav.set({ page: { kind: 'front' }, stack: [], y: 0, tick: cur.tick + 1 })
}

export const openStoreGame = (key: StoreKey) => go({ kind: 'game', key })
export const openWishlist = () => go({ kind: 'wishlist' })
/** Store settings live in Settings › Store. */
export const openStoreSettings = () => openSettings('store')

export function storeBack() {
  const cur = storeNav.get()
  const prev = cur.stack[cur.stack.length - 1]
  if (!prev) { storeHome(); return }
  storeNav.set({ page: prev.page, stack: cur.stack.slice(0, -1), y: prev.y, tick: cur.tick + 1 })
}

/** The store bar's search and filters; kept here so a game page and the way back do not lose them. */
interface Search { q: string; filters: StoreFilters }
export const storeSearch = createStore<Search>({ q: '', filters: {} })
export const useStoreSearch = <S>(select: (s: Search) => S) => useStore(storeSearch, select)
