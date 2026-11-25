import { useGames } from '../lib/games'
import { useSettings } from '../lib/settings'
import { openPalette, uiStore, useUi } from '../lib/ui'
import { tether, useTether } from '../tether'
import { openStore, storeHome } from '../lib/store-nav'
import { Avatar } from './Avatar'
import { BrandMark } from './Brand'
import { DownloadRing } from './downloads/DownloadRing'
import { InvitesBar } from './Invites'
import { SearchIcon } from './Icons'
import { Hint } from './PadGlyph'

export function goHome(scroller: HTMLElement | null) {
  uiStore.set({ route: 'home', menu: false })
  scroller?.scrollTo({ top: 0, behavior: 'smooth' })
}

export function goLibrary(scroller: HTMLElement | null) {
  uiStore.set({ route: 'home', menu: false })
  const lib = document.getElementById('lib')
  // After the Home page is back in the DOM.
  requestAnimationFrame(() => (scroller ?? document.querySelector('.app'))?.scrollTo({ top: (lib?.offsetTop ?? 600) - 70, behavior: 'smooth' }))
}

export function TopBar({ scroller }: { scroller: React.RefObject<HTMLDivElement | null> }) {
  const route = useUi((s) => s.route)
  const scrolled = useUi((s) => s.scrolled)
  const onLibrary = useUi((s) => s.onLibrary)
  const t = useTether((s) => s)
  const loaded = useGames((s) => s.loaded)
  const playing = t.signedIn ? t.friends.filter((f) => f.playing) : []
  // The store's hero sits under a frosted bar (mockups/06-store.html); Home and Collection start clear.
  const clear = route === 'collection' || (route === 'home' && !scrolled)
  const home = route === 'home' && !onLibrary
  const lib = route === 'home' && onLibrary
  return (
    <header className={`bar glass${clear ? ' clear' : ''}`} data-tauri-drag-region>
      <span className="brand" data-tauri-drag-region><BrandMark size={30} /></span>
      <button className={`nav${home ? ' on' : ''}`} onClick={() => goHome(scroller.current)}>Home</button>
      <button className={`nav${lib ? ' on' : ''}`} onClick={() => goLibrary(scroller.current)}>Library</button>
      <button className={`nav${route === 'collection' ? ' on' : ''}`} onClick={() => uiStore.set({ route: 'collection', menu: false })}>Collection</button>
      <button className={`nav${route === 'store' ? ' on' : ''}`} onClick={() => (route === 'store' ? storeHome() : openStore())}>Store</button>
      <button className="nav dim" title="Coming later" tabIndex={-1}>Stats</button>
      <button className="search" onClick={() => openPalette()} disabled={!loaded}>
        <SearchIcon />Type to find a game<Hint action="search" keyText="/" />
      </button>
      {playing.length > 0 && (
        <div className="fpill" title="from tether">
          <span className="avs">{playing.slice(0, 3).map((f) => <Avatar key={f.id} f={f} />)}</span>
          {playing.length} playing
        </div>
      )}
      <InvitesBar />
      <DownloadRing />
      <button className={`me${route === 'settings' ? ' on' : ''}`} title="Settings" onClick={() => uiStore.set((s) => ({ menu: !s.menu }))}>
        <Avatar f={t.me} noTitle>
          {t.signedIn && t.connection === 'online' && <span className="dot" />}
        </Avatar>
      </button>
    </header>
  )
}

export function FriendsJoin() {
  const t = useTether((s) => s)
  const route = useUi((s) => s.route)
  const nearTop = useUi((s) => s.nearTop)
  const onHome = useSettings((s) => s.friends_on_home)
  const g = t.signedIn && onHome ? tether.playingGroups(t)[0] : undefined
  if (!g) return null
  const mins = Math.max(1, Math.round((Date.now() / 1000 - g.since) / 60))
  const names = g.friends.map((f) => f.name)
  const who = names.length > 1 ? `${names.slice(0, -1).join(', ')} and ${names[names.length - 1]}` : names[0]
  return (
    <div className={`join glass${route !== 'home' || !nearTop ? ' hide' : ''}`}>
      <span className="avs">{g.friends.slice(0, 3).map((f) => <Avatar key={f.id} f={f} />)}</span>
      <span><b>{who}</b> {names.length > 1 ? 'are' : 'is'} in {g.name} · {mins}m</span>
    </div>
  )
}
