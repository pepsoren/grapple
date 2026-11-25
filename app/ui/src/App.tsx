import { lazy, Suspense, useEffect, useRef } from 'react'
import { AddGame } from './components/AddGame'
import { Drawer } from './components/Drawer'
import { InstallSheet } from './components/downloads/InstallSheet'
import { Home, useShelf } from './components/Home'
import { LaunchCard, NowPlaying, PlayingCard } from './components/Launch'
import { Palette } from './components/Palette'
import { SettingsMenu } from './components/SettingsMenu'
import { GameMenu } from './components/GameMenu'
import { InviteOpen, InvitePicker, JoinedToast } from './components/Invites'
import { SignIn } from './components/SignIn'
import { FriendsJoin, TopBar, goHome, goLibrary } from './components/TopBar'
import { select, useGames } from './lib/games'
import { expand, play, useSession } from './lib/session'
import { openStore } from './lib/store-nav'
import { closePalette, closeSettings, openPalette, openSettings, setDrawer, uiStore, useUi } from './lib/ui'
import { startPadNav } from './lib/padnav'
import './styles/drawer.css'

const Collection = lazy(() => import('./components/Collection'))
const StoreRoot = lazy(() => import('./components/store/StoreRoot'))
const DownloadsPage = lazy(() => import('./components/downloads/DownloadsPage'))
const SettingsPage = lazy(() => import('./components/settings/SettingsPage'))
const GameSheet = lazy(() => import('./components/settings/GameSheet'))

export default function App() {
  const scroller = useRef<HTMLDivElement>(null)
  const route = useUi((s) => s.route)
  const palette = useUi((s) => s.palette)
  const query = useUi((s) => s.paletteQuery)
  const toastMsg = useUi((s) => s.toast)
  const phase = useSession((s) => s.phase)
  const compact = useSession((s) => s.compact)
  const shelf = useShelf()
  useEffect(startPadNav, [])
  const sel = useGames((s) => s.selected)
  const gameSheet = useUi((s) => s.gameSheet !== null)

  // Keys on the page (the palette and the Collection handle their own while open).
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const ui = uiStore.get()
      // Ctrl+, Settings; Ctrl+1-4 Home, Library, Collection, Store (from anywhere, typing included).
      if (!compact && (e.ctrlKey || e.metaKey) && !e.altKey && !e.shiftKey && !ui.addGame) {
        const page = ['1', '2', '3', '4'].indexOf(e.key)
        if (e.key === ',' || page >= 0) {
          e.preventDefault()
          uiStore.set({ palette: false, gameSheet: null })
          if (e.key === ',') openSettings()
          else if (page === 0) goHome(scroller.current)
          else if (page === 1) goLibrary(scroller.current)
          else if (page === 2) uiStore.set({ route: 'collection', menu: false })
          else openStore()
          return
        }
      }
      const s = (document.activeElement as HTMLElement | null)?.tagName
      if (s === 'INPUT' || s === 'TEXTAREA') return
      if (compact) {
        if (e.key === 'Escape') expand()
        return
      }
      if (ui.palette || ui.addGame || ui.gameSheet) return
      if (e.key === 'Escape') {
        if (ui.menu) uiStore.set({ menu: false })
        else if (ui.drawer) setDrawer(false)
        else if (ui.route === 'settings') closeSettings()
        return
      }
      if (ui.route !== 'home') return
      if (phase === 'launching') return
      if (e.key === 'ArrowRight' || e.key === 'ArrowLeft') {
        e.preventDefault()
        if (!shelf.length) return
        const i = Math.max(0, shelf.findIndex((g) => g.id === sel))
        select(shelf[(i + (e.key === 'ArrowRight' ? 1 : -1) + shelf.length) % shelf.length].id)
      } else if (e.key === 'Enter') {
        if (sel) play(sel)
      } else if (e.key === 'Tab') {
        e.preventDefault()
        setDrawer(!ui.drawer)
      } else if (e.key === '/') {
        e.preventDefault()
        openPalette()
      } else if (e.key.length === 1 && /[a-z0-9!']/i.test(e.key) && !e.ctrlKey && !e.metaKey && !e.altKey) {
        e.preventDefault()
        openPalette(e.key)
      }
    }
    document.addEventListener('keydown', onKey)
    return () => document.removeEventListener('keydown', onKey)
  }, [compact, phase, shelf, sel])

  // Typing on the Collection page goes to its own search, so only open the palette from Home.
  useEffect(() => {
    if (route === 'collection') closePalette()
  }, [route])

  return (
    <div className={`win${compact ? ' small' : ''}`}>
      {!compact && (
        <>
          {route === 'home' && <Home scroller={scroller} />}
          {route === 'collection' && (
            <Suspense fallback={null}>
              <Collection />
            </Suspense>
          )}
          {route === 'store' && (
            <Suspense fallback={null}>
              <StoreRoot scroller={scroller} />
            </Suspense>
          )}
          {route === 'downloads' && (
            <Suspense fallback={null}>
              <DownloadsPage />
            </Suspense>
          )}
          {route === 'settings' && (
            <Suspense fallback={null}>
              <SettingsPage />
            </Suspense>
          )}
          <TopBar scroller={scroller} />
          <FriendsJoin />
          <Drawer />
          {palette && (
            <Palette
              initial={query}
              onClose={closePalette}
              onPlay={(id) => { closePalette(); setDrawer(false); select(id); play(id) }}
              onDetails={(id) => { closePalette(); uiStore.set({ route: 'home' }); select(id); setDrawer(true) }}
            />
          )}
          <LaunchCard />
          {phase === 'playing' && <NowPlaying />}
          <SettingsMenu />
          <GameMenu />
          {gameSheet && (
            <Suspense fallback={null}>
              <GameSheet />
            </Suspense>
          )}
          <AddGame />
          <InstallSheet />
          <SignIn />
          <InvitePicker />
          <InviteOpen />
          <JoinedToast />
          {toastMsg && <div className="toast">{toastMsg}</div>}
        </>
      )}
      {compact && phase === 'playing' && <PlayingCard />}
      {compact && phase !== 'playing' && <LaunchCard />}
    </div>
  )
}
