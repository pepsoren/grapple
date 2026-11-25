// Dev-only (imported under import.meta.env.DEV, so it is not in release builds): ?state=<name> puts the UI in a
// state for headless screenshots: collection, collection-game, drawer, palette, launching, playing, downloads,
// downloads-empty, download-settings, install (&game=<id>), settings (&section=<id>), game-settings (&game=<id>&tab=<tab>),
// menu; &scale=<percent> sets the interface size, &gamemenu=<id> opens a game's right-click menu at 600,300.
import { gamesStore, select } from './games'
import { handleLaunchEvent, sessionStore } from './session'
import { openGameSheet, openPalette, openSettings, setDrawer, uiStore, type GameSheetTab, type SettingsSection } from './ui'
import { gameById } from './games'

export function applyDevState() {
  const state = new URLSearchParams(location.search).get('state')
  if (!state) return
  const ready = () => gamesStore.get().loaded
  const t = setInterval(() => {
    if (!ready()) return
    clearInterval(t)
    const id = (new URLSearchParams(location.search).get('game')) ?? 'steam:730'
    if (state === 'collection') uiStore.set({ route: 'collection' })
    if (state === 'collection-game') { uiStore.set({ route: 'collection' }); void import('../components/Collection').then(() => (window as unknown as { __pick?: (i: string) => void }).__pick?.(id)) }
    if (state === 'drawer') { select(id); setDrawer(true) }
    if (state === 'palette') openPalette('g')
    if (state === 'downloads' || state === 'downloads-empty') uiStore.set({ route: 'downloads' })
    if (state === 'download-settings') openSettings('downloads')
    if (state === 'settings') openSettings((new URLSearchParams(location.search).get('section') ?? 'general') as SettingsSection)
    if (state === 'game-settings') openGameSheet(id, (new URLSearchParams(location.search).get('tab') ?? 'general') as GameSheetTab)
    if (state === 'menu') uiStore.set({ menu: true })
    const scale = new URLSearchParams(location.search).get('scale')
    if (scale) void import('./settings').then((m) => m.updateSettings({ ui_scale: Number(scale) }))
    const gm = new URLSearchParams(location.search).get('gamemenu')
    if (gm) uiStore.set({ gameMenu: { id: gm, x: 600, y: 300 } })
    if (state === 'install') { const g = gameById(id); if (g) void import('./downloads').then((m) => m.installGame(g)) }
    if (state === 'launching') { select(id); sessionStore.set({ phase: 'launching', game: id }); handleLaunchEvent({ kind: 'step', label: 'Fetching game from Steam', done: true }); handleLaunchEvent({ kind: 'step', label: 'Game files ready', done: false }) }
    if (state === 'playing') { select(id); sessionStore.set({ phase: 'playing', game: id, startedAt: Math.floor(Date.now() / 1000) - 3723, compact: new URLSearchParams(location.search).has('compact') }) }
  }, 50)
}
