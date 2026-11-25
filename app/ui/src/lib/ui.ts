// Transient UI state: which page, drawer, palette, menu, toast.
import { createStore, useStore } from './store'

export type Route = 'home' | 'collection' | 'store' | 'downloads' | 'settings'
export type GameSheetTab = 'general' | 'launch' | 'controller' | 'linux' | 'files'
export type SettingsSection =
  | 'general' | 'accounts' | 'appearance' | 'library' | 'playing' | 'controllers' | 'downloads' | 'store' | 'social'
  | 'notifications' | 'shortcuts' | 'advanced'

interface UiState {
  route: Route
  drawer: boolean
  palette: boolean
  paletteQuery: string
  menu: boolean
  /** The "Add any game" dialog. */
  addGame: boolean
  toast: string | null
  /** A game's right-click menu, at the pointer. */
  gameMenu: { id: string; x: number; y: number } | null
  /** The Settings page: its section, and the page to go back to. */
  settings: SettingsSection
  settingsFrom: Route
  /** A game's settings sheet (mockups/08-settings.html, screen C), on a tab. */
  gameSheet: { id: string; tab: GameSheetTab } | null
  /** Home scroller position, for the top bar and friends bar. */
  scrolled: boolean
  onLibrary: boolean
  nearTop: boolean
}

export const uiStore = createStore<UiState>({
  route: 'home', drawer: false, palette: false, paletteQuery: '', menu: false, addGame: false, toast: null, gameMenu: null,
  settings: 'general', settingsFrom: 'home', gameSheet: null,
  scrolled: false, onLibrary: false, nearTop: true,
})
export const useUi = <S>(select: (s: UiState) => S) => useStore(uiStore, select)

export const openPalette = (q = '') => uiStore.set({ palette: true, paletteQuery: q, menu: false })
export const closePalette = () => uiStore.set({ palette: false })
export const openAddGame = () => uiStore.set({ addGame: true, menu: false, palette: false })
export const closeAddGame = () => uiStore.set({ addGame: false })
export const setDrawer = (drawer: boolean) => uiStore.set({ drawer, menu: false })
export const openDownloads = () => uiStore.set({ route: 'downloads', menu: false, drawer: false, palette: false })
export const openDownloadSettings = () => openSettings('downloads')

/** The Settings page (Ctrl+,), on a section; Esc or back returns to the page you came from. */
export function openSettings(section?: SettingsSection) {
  const cur = uiStore.get()
  uiStore.set({
    route: 'settings',
    settingsFrom: cur.route === 'settings' ? cur.settingsFrom : cur.route,
    settings: section ?? cur.settings,
    menu: false, drawer: false, palette: false, gameMenu: null,
  })
}
export const closeSettings = () => uiStore.set((s) => ({ route: s.settingsFrom }))

export const openGameSheet = (id: string, tab: GameSheetTab = 'general') => uiStore.set({ gameSheet: { id, tab }, gameMenu: null, menu: false, palette: false })
export const closeGameSheet = () => uiStore.set({ gameSheet: null })

/** Right-click on a game: its menu at the pointer. */
export function openGameMenu(e: { preventDefault: () => void; stopPropagation: () => void; clientX: number; clientY: number }, id: string) {
  e.preventDefault()
  e.stopPropagation()
  uiStore.set({ gameMenu: { id, x: e.clientX, y: e.clientY }, menu: false })
}
export const closeGameMenu = () => uiStore.set({ gameMenu: null })

let toastTimer = 0
export function toast(message: string) {
  uiStore.set({ toast: message })
  clearTimeout(toastTimer)
  toastTimer = window.setTimeout(() => uiStore.set({ toast: null }), 2600)
}
