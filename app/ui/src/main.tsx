import '@fontsource/figtree/latin-400.css'
import '@fontsource/figtree/latin-500.css'
import '@fontsource/figtree/latin-600.css'
import '@fontsource/figtree/latin-700.css'
import '@fontsource/figtree/latin-800.css'
import './styles/tokens.css'
import './styles/base.css'
import './styles/home.css'
import './styles/chrome.css'
import { useEffect } from 'react'
import { createRoot } from 'react-dom/client'
import { api, inTauri } from './lib/ipc'
import { startDownloads } from './lib/downloads'
import { gamesStore, startGames } from './lib/games'
import { goLibrary } from './components/TopBar'
import { startSession } from './lib/session'
import { loadSettings, settingsStore } from './lib/settings'
import { openStore } from './lib/store-nav'
import { uiStore } from './lib/ui'
import { startTether } from './tether'
import { startInvites } from './lib/invites'
import { initTheme, injectThemeOverride } from './lib/theme'
import App from './App'

initTheme()

// Page errors go to the process log (Tauri's webview has no console you can see).
if (inTauri) {
  const log = (msg: string) => void import('@tauri-apps/api/core').then((c) => c.invoke('ui_log', { msg }))
  window.addEventListener('error', (e) => log(`error: ${e.message} @ ${e.filename}:${e.lineno}`))
  window.addEventListener('unhandledrejection', (e) => log(`rejection: ${String(e.reason)}`))
}

// Start the data requests before React renders, so they overlap with the bundle work.
startGames()
void injectThemeOverride()
startSession()
// "Open on" (Settings › General): the first page, unless something else was opened meanwhile.
void loadSettings().then(() => {
  const ui = uiStore.get()
  if (ui.route !== 'home' || ui.drawer || ui.palette) return
  const page = settingsStore.get().open_on
  if (page === 'collection') uiStore.set({ route: 'collection' })
  else if (page === 'store') openStore()
  else if (page === 'library') {
    // After the library is on the page, so there is somewhere to scroll to.
    const go = () => requestAnimationFrame(() => { if (uiStore.get().route === 'home') goLibrary(null) })
    if (gamesStore.get().loaded) go()
    else {
      const off = gamesStore.subscribe(() => { if (gamesStore.get().loaded) { off(); go() } })
    }
  }
})
void startTether()
void startInvites()
startDownloads()

/** After the first commit: tell Rust the UI is rendered (it shows the window), then report the first frame.
 *  A hidden window gets no animation frames, so the rAF below fires once the window is really on screen. */
function Ready() {
  useEffect(() => {
    void api.uiReady()
    requestAnimationFrame(() => void api.shown())
    if (inTauri) {
      document.addEventListener('visibilitychange', () => {
        if (document.visibilityState === 'visible') requestAnimationFrame(() => void api.shown())
      })
    }
  }, [])
  return null
}

if (import.meta.env.DEV) void import('./lib/devstate').then((m) => m.applyDevState())

createRoot(document.getElementById('root')!).render(
  <>
    <App />
    <Ready />
  </>,
)

