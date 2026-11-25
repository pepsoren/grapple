import { useEffect, useMemo, useRef, useState } from 'react'
import { ago, hm, hoursLong, storeLabel } from '../lib/format'
import { byRecent, select, useGames } from '../lib/games'
import { play } from '../lib/session'
import { openAddGame, openGameMenu, setDrawer, uiStore } from '../lib/ui'
import type { Game, Store } from '../lib/types'
import { tether, useTether } from '../tether'
import { Avatar } from './Avatar'
import { Cover, backdrop, url } from './Art'
import { DotsIcon, PlayIcon, PlusIcon, TetherIcon } from './Icons'
import { InviteButton } from './Invites'
import { HomePicks } from './store/HomePicks'
import { useSettings } from '../lib/settings'

const SHELF = 10

/** Installed games, most recently played first: what Home shows. */
export function useInstalled(): Game[] {
  const games = useGames((s) => s.games)
  return useMemo(() => games.filter((g) => g.installed).sort(byRecent), [games])
}

/** The "Jump back in" list: the most recent, with the selected game kept in view. */
export function useShelf(): Game[] {
  const installed = useInstalled()
  const sel = useGames((s) => s.selected)
  return useMemo(() => {
    const top = installed.slice(0, SHELF)
    if (sel && !top.some((g) => g.id === sel)) {
      const g = installed.find((x) => x.id === sel)
      if (g) return [g, ...top.slice(0, SHELF - 1)]
    }
    return top
  }, [installed, sel])
}

function HeroBg({ game }: { game: Game | undefined }) {
  const bd = game ? backdrop(game) : { url: null, blur: false }
  const [slots, setSlots] = useState<{ url: string | null; blur: boolean }[]>([bd, { url: null, blur: false }])
  const [front, setFront] = useState(0)
  useEffect(() => {
    if (slots[front].url === bd.url) return
    const back = 1 - front
    const img = new Image()
    const swap = () => {
      setSlots((s) => s.map((x, i) => (i === back ? bd : x)))
      setFront(back)
    }
    img.onload = img.onerror = swap
    if (bd.url) img.src = bd.url
    else swap()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [bd.url])
  return (
    <>
      {slots.map((s, i) => (
        <div
          key={i}
          className={`bgl${i === front ? ' front' : ''}${s.blur ? ' blur' : ''}`}
          style={s.url ? { backgroundImage: `url("${s.url}")` } : undefined}
        />
      ))}
    </>
  )
}

function Bars({ days, h }: { days: number[] | undefined; h: number }) {
  const d = days ?? Array(14).fill(0)
  const max = Math.max(60, ...d)
  return (
    <div className="bars" style={{ height: h }}>
      {d.map((v, i) => (
        <div key={i} className={v ? '' : 'zero'} style={v ? { height: Math.max(6, (v / max) * h) } : undefined} />
      ))}
    </div>
  )
}
export { Bars }

function Hero({ game }: { game: Game | undefined }) {
  const stats = useGames((s) => (game ? s.stats[game.id] : undefined))
  const showFriends = useSettings((s) => s.friends_on_home)
  const note = useTether((s) => (game && s.signedIn ? s.notes[game.id] : undefined))
  if (!game) {
    return (
      <section className="hero">
        <div className="empty-hero">
          <b style={{ fontSize: 20, color: 'var(--text-strong)' }}>No installed games yet</b>
          grapple is looking through Steam and Epic. Your whole list is in Collection.
        </div>
      </section>
    )
  }
  const logo = url(game, 'logo')
  const total = stats?.total_min ?? game.playtime_min
  const avg = stats && stats.sessions ? Math.round(stats.total_min / stats.sessions) : 0
  return (
    <section className="hero">
      <HeroBg game={game} />
      <div className="shade" />
      <div className="info">
        <div className="logo-box">{logo ? <img src={logo} alt={game.name} /> : <div className="logo-text">{game.name}</div>}</div>
        <div className="stats">
          <span><b>{total ? hoursLong(total) : '—'}</b><br />played</span>
          <span><b>{ago(game.last_played)}</b><br />last played</span>
          <span><b>{stats ? hm(stats.this_week_min) : '—'}</b><br />this week</span>
        </div>
        <div className="actions">
          <button className="btn play" onClick={() => play(game.id)}><PlayIcon />Play</button>
          <InviteButton game={game.id} />
          <button className="btn ghost" title="Details (Tab)" onClick={() => setDrawer(!uiStore.get().drawer)}><DotsIcon /></button>
          {note && showFriends && <span className="friend-note"><TetherIcon className="accent" />{note}</span>}
        </div>
      </div>
      <div className="side glass">
        <div className="head">Last 14 days<span>{stats?.sessions ?? 0} sessions</span></div>
        <Bars days={stats?.last_14_days} h={56} />
        <div className="foot">
          <span>Longest <b>{stats ? hm(stats.longest_min) : '—'}</b></span>
          <span>Avg <b>{avg ? hm(avg) : '—'}</b></span>
        </div>
      </div>
    </section>
  )
}

function Shelf({ shelf, sel }: { shelf: Game[]; sel: string | null }) {
  const friends = useTether((s) => s)
  const showFriends = useSettings((s) => s.friends_on_home)
  // Warm the hero images of the shelf so arrow keys crossfade without waiting.
  useEffect(() => {
    shelf.forEach((g) => {
      const u = backdrop(g).url
      if (u) new Image().src = u
    })
  }, [shelf])
  return (
    <section className="shelf">
      <div className="h2">Jump back in</div>
      <div className="row">
        {shelf.map((g) => {
          const live = friends.signedIn && showFriends ? tether.friendsIn(friends, g.id) : []
          return (
            <button key={g.id} data-game={g.id} className={`tile${g.id === sel ? ' on' : ''}`} onClick={() => select(g.id)} onContextMenu={(e) => openGameMenu(e, g.id)} title={g.name}>
              <Cover g={g} />
              {g.store !== 'steam' && <span className="src">{storeLabel(g).toUpperCase()}</span>}
              {live.length > 0 && <span className="live">{live.map((f) => <Avatar key={f.id} f={f} />)}</span>}
            </button>
          )
        })}
        <button className="add-circle" title="Add any game" onClick={openAddGame}><PlusIcon /></button>
      </div>
    </section>
  )
}

type Filter = 'All' | Store

function Library({ games, sel }: { games: Game[]; sel: string | null }) {
  const [filter, setFilter] = useState<Filter>('All')
  const lib = useGames((s) => s.library)
  const top = useGames((s) => (lib?.most_played ? s.games.find((g) => g.id === lib.most_played) : undefined))
  const count = (s: Store) => games.filter((g) => g.store === s).length
  const list = filter === 'All' ? games : games.filter((g) => g.store === filter)
  const tabs: [Filter, string, number][] = [
    ['All', 'All', games.length], ['steam', 'Steam', count('steam')], ['epic', 'Epic', count('epic')], ['manual', 'Other', count('manual')],
  ]
  return (
    <section className="lib" id="lib">
      <div className="head">
        <div className="title">Library</div>
        {tabs.map(([k, label, n]) => (
          <button key={k} className={`tab${filter === k ? ' on' : ''}`} onClick={() => setFilter(k)}>{label} {n}</button>
        ))}
        <span className="sub">Sort: recently played</span>
      </div>
      <div className="grid">
        {list.map((g) => (
          <div
            key={g.id}
            data-game={g.id}
            className={`card${g.id === sel ? ' on' : ''}`}
            onContextMenu={(e) => openGameMenu(e, g.id)}
            onClick={() => {
              select(g.id)
              document.querySelector('.app')?.scrollTo({ top: 0, behavior: 'smooth' })
            }}
          >
            <div className="cv">
              <Cover g={g} lazy />
              {g.store !== 'steam' && <span className="src">{storeLabel(g).toUpperCase()}</span>}
              <div className="ov">
                <button
                  className="pb"
                  title={`Play ${g.name}`}
                  onClick={(e) => {
                    e.stopPropagation()
                    select(g.id)
                    play(g.id)
                  }}
                >
                  <PlayIcon size={16} />
                </button>
              </div>
            </div>
            <div className="t">{g.name}</div>
            <div className="sub">{g.playtime_min ? hoursLong(g.playtime_min) : 'Never played'} · {ago(g.last_played)}</div>
          </div>
        ))}
        <div className="card add" onClick={openAddGame}>
          <div className="cv">
            <PlusIcon size={26} />
            Add any game
            <span>.exe, AppImage,<br />emulator</span>
          </div>
        </div>
      </div>
      <div className="totals">
        <div className="stat"><span className="sub">Played this week</span><b>{lib ? hm(lib.this_week_min) : '—'}</b></div>
        <div className="stat"><span className="sub">Played in the last 14 days</span><b>{lib ? hm(lib.last_14_days_min) : '—'}</b></div>
        <div className="stat"><span className="sub">Most played</span><b>{top?.name ?? '—'}</b></div>
        <div className="stat"><span className="sub">All time, all stores</span><b>{lib ? `${Math.round(lib.all_time_min / 60)}h` : '—'}</b></div>
      </div>
    </section>
  )
}

export function Home({ scroller }: { scroller: React.RefObject<HTMLDivElement | null> }) {
  const installed = useInstalled()
  const shelf = useShelf()
  const sel = useGames((s) => s.selected)
  const game = useGames((s) => s.games.find((g) => g.id === s.selected)) ?? installed[0]
  const raf = useRef(0)
  const onScroll = () => {
    if (raf.current) return
    raf.current = requestAnimationFrame(() => {
      raf.current = 0
      const y = scroller.current?.scrollTop ?? 0
      uiStore.set({ scrolled: y >= 40, onLibrary: y >= 380, nearTop: y <= 300 })
    })
  }
  return (
    <div className="app" ref={scroller} onScroll={onScroll}>
      <Hero game={game} />
      <Shelf shelf={shelf} sel={sel} />
      <HomePicks />
      <Library games={installed} sel={sel} />
    </div>
  )
}
