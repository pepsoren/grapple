import '../styles/collection.css'
import { useEffect, useMemo, useRef } from 'react'
import { ago, bucket, hoursLong, hoursShort, nowSec, storeLabel } from '../lib/format'
import { gameById, loadStats, select, toggleFavourite, useGames } from '../lib/games'
import { createStore, useStore } from '../lib/store'
import { installGame, removeManual } from '../lib/library'
import { play } from '../lib/session'
import type { Game } from '../lib/types'
import { tether, useTether } from '../tether'
import { Avatar } from './Avatar'
import { Cover, Icon, backdrop, url } from './Art'
import { Bars } from './Home'
import { UninstallButton } from './downloads/Uninstall'
import { DownloadIcon, HomeIcon, PlayIcon, SearchIcon, StarIcon } from './Icons'
import { openGameMenu } from '../lib/ui'

type Filter = 'All' | 'Installed' | 'Played' | 'Never played' | 'Epic'

// Module-level so leaving and returning to the page keeps your place.
const cStore = createStore<{ sel: string | null; filter: Filter; shut: Record<string, boolean>; q: string }>({
  sel: null, filter: 'All', shut: {}, q: '',
})

if (import.meta.env.DEV) (window as unknown as { __pick?: (i: string) => void }).__pick = (id) => cStore.set({ sel: id })

const sortAlpha = (a: Game, b: Game) => a.sort_name.localeCompare(b.sort_name)

function passes(g: Game, q: string, filter: Filter) {
  if (q && !g.name.toLowerCase().includes(q)) return false
  switch (filter) {
    case 'Installed': return g.installed
    case 'Played': return g.playtime_min > 0
    case 'Never played': return g.playtime_min === 0
    case 'Epic': return g.store === 'epic'
    default: return true
  }
}

function statusLabel(g: Game): string | null {
  switch (g.status?.kind) {
    case 'update_queued': return 'Update queued'
    case 'updating': return 'Updating'
    case 'running': return 'Running'
    default: return null
  }
}

function Row({ g, on, onPick }: { g: Game; on: boolean; onPick: (id: string) => void }) {
  const t = useTether((s) => s)
  const fr = t.signedIn ? tether.friendsIn(t, g.id) : []
  const st = statusLabel(g)
  return (
    <div className={`crow${g.installed ? '' : ' off'}${on ? ' on' : ''}`} data-c={g.id} onClick={() => onPick(g.id)} onContextMenu={(e) => openGameMenu(e, g.id)}>
      <Icon g={g} />
      <span className="nm">{g.name}</span>
      {g.store === 'epic' && <span className="tag">EPIC</span>}
      {st ? <span className="hr st">{st}</span>
        : fr.length ? <span className="hr fr">{fr.map((f) => <Avatar key={f.id} f={f} />)}</span>
          : <span className="hr">{hoursShort(g.playtime_min)}</span>}
    </div>
  )
}

function Group({ id, title, list, sel, onPick }: { id: string; title: string; list: Game[]; sel: string | null; onPick: (id: string) => void }) {
  const shut = useStore(cStore, (s) => !!s.shut[id])
  if (!list.length) return null
  return (
    <>
      <div className={`grp${shut ? ' shut' : ''}`} onClick={() => cStore.set((s) => ({ shut: { ...s.shut, [id]: !s.shut[id] } }))}>
        <span className="ch">▾</span>{title}<span className="ct">{list.length}</span>
      </div>
      {!shut && list.map((g) => <Row key={g.id} g={g} on={sel === g.id} onPick={onPick} />)}
    </>
  )
}

function Mini({ g, onPick }: { g: Game; onPick: (id: string) => void }) {
  return (
    <div className="mini" onClick={() => onPick(g.id)} onContextMenu={(e) => openGameMenu(e, g.id)}>
      <div className="c"><Cover g={g} lazy /></div>
      <div className="t">{g.name}</div>
      <div className="sub">{hoursShort(g.playtime_min) || 'New'}</div>
    </div>
  )
}

function Overview({ games, onPick }: { games: Game[]; onPick: (id: string) => void }) {
  const d = useMemo(() => {
    const total = games.reduce((a, g) => a + g.playtime_min, 0)
    const never = games.filter((g) => !g.playtime_min)
    const rec = games.filter((g) => g.last_played).sort((a, b) => b.last_played! - a.last_played!).slice(0, 14)
    const groups: { b: string; l: Game[] }[] = []
    rec.forEach((g) => {
      const b = bucket(g.last_played!)
      let x = groups.find((y) => y.b === b)
      if (!x) groups.push((x = { b, l: [] }))
      x.l.push(g)
    })
    const top = games.filter((g) => g.playtime_min).sort((a, b) => b.playtime_min - a.playtime_min).slice(0, 10)
    return { total, never, groups, top, max: top[0]?.playtime_min ?? 1, nev: never.filter((g) => g.art.cover).slice(0, 12) }
  }, [games])
  return (
    <>
      <div className="sec" style={{ paddingTop: 26 }}>
        <div className="big"><b>Your collection</b><span className="sub">Steam + Epic, one list</span></div>
        <div className="stats4">
          <div className="stat"><span className="sub">Games</span><b>{games.length}</b></div>
          <div className="stat"><span className="sub">Installed</span><b>{games.filter((g) => g.installed).length}</b></div>
          <div className="stat"><span className="sub">Hours played</span><b>{Math.round(d.total / 60).toLocaleString()}h</b></div>
          <div className="stat"><span className="sub">Never played</span><b>{d.never.length}</b></div>
        </div>
      </div>
      <div className="sec">
        <h3>Recent</h3>
        <div className="tline">
          {d.groups.map((x) => (
            <div className="tgrp" key={x.b}>
              <div className="lbl">{x.b}</div>
              <div className="cvs">{x.l.map((g) => <Mini key={g.id} g={g} onPick={onPick} />)}</div>
            </div>
          ))}
        </div>
      </div>
      <div className="sec">
        <h3>Most played <span>all time</span></h3>
        {d.top.map((g) => (
          <div className="mp" key={g.id} onClick={() => onPick(g.id)}>
            <Icon g={g} />
            <span className="nm">{g.name}</span>
            <span className="bar2"><i style={{ width: `${(g.playtime_min / d.max) * 100}%` }} /></span>
            <span className="h">{hoursShort(g.playtime_min)}</span>
          </div>
        ))}
      </div>
      <div className="sec" style={{ paddingBottom: 28 }}>
        <h3>Never played <span>{d.never.length} games you own but haven't started</span></h3>
        <div className="tline">{d.nev.map((g) => <Mini key={g.id} g={g} onPick={onPick} />)}</div>
      </div>
    </>
  )
}

function GamePage({ g }: { g: Game }) {
  const stats = useGames((s) => s.stats[g.id])
  const t = useTether((s) => s)
  const fr = t.signedIn ? tether.friendsIn(t, g.id) : []
  useEffect(() => { void loadStats(g.id) }, [g.id])
  const hero = url(g, 'hero')
  const bd = backdrop(g)
  const st = statusLabel(g)
  const recent = g.playtime_min > 0 && g.last_played != null && nowSec() - g.last_played < 14 * 86400
  const names = fr.map((f) => f.name)
  return (
    <>
      <div className="gp-hero">
        {hero ? <div className="bg" style={{ backgroundImage: `url("${hero}")` }} /> : (
          <>
            {bd.url && <div className="bg blur" style={{ backgroundImage: `url("${bd.url}")` }} />}
            {url(g, 'cover') && <img className="cover" src={url(g, 'cover')!} alt="" />}
          </>
        )}
        <div className="fade" />
        <div className="name">{g.name}</div>
      </div>
      <div className="gp-bar">
        {g.installed ? (
          <button className="btn play" onClick={() => { select(g.id); play(g.id) }}><PlayIcon />Play</button>
        ) : (
          <button className="btn play neutral" onClick={() => void installGame(g)}><DownloadIcon />Install</button>
        )}
        <span className="k">Played<br /><b>{hoursLong(g.playtime_min)}</b></span>
        <span className="k">Last played<br /><b>{ago(g.last_played)}</b></span>
        <span className="chip">
          <i style={{ color: g.store === 'epic' ? 'var(--store-epic)' : 'var(--store-steam)' }} />
          {storeLabel(g)}{g.installed ? '' : ' · not installed'}
        </span>
        <button className="btn ghost" title="Favourite" onClick={() => void toggleFavourite(g.id)}><StarIcon on={g.favourite} /></button>
        {g.store === 'manual' && <button className="btn ghost" title="Remove from grapple (the program itself is not touched)" onClick={() => void removeManual(g)}>Remove</button>}
        <UninstallButton g={g} />
      </div>
      <div className="sec">
        {st && <div className="gp-banner">{st}: Steam will update it in the background before it launches.</div>}
        {fr.length > 0 && (
          <div className="gp-friends">
            <span className="avs">{fr.map((f) => <Avatar key={f.id} f={f} />)}</span>
            <span><b>{names.length > 1 ? `${names.slice(0, -1).join(', ')} and ${names[names.length - 1]} are` : `${names[0]} is`}</b> playing this now</span>
          </div>
        )}
        {recent ? (
          <>
            <h3 style={{ margin: '0 0 12px', fontSize: 15, color: 'var(--text-strong)' }}>Last 14 days</h3>
            <div style={{ maxWidth: 520 }}><Bars days={stats?.last_14_days} h={64} /></div>
          </>
        ) : g.playtime_min ? (
          <div className="sub gp-text">Not played in the last two weeks.</div>
        ) : (
          <div className="sub gp-text">
            You own this but haven't played it yet.
          </div>
        )}
      </div>
    </>
  )
}

export default function Collection() {
  const games = useGames((s) => s.games)
  const { sel, filter, q } = useStore(cStore, (s) => s)
  const main = useRef<HTMLElement>(null)
  const list = useRef<HTMLDivElement>(null)
  const search = useRef<HTMLInputElement>(null)
  const pick = (id: string | null) => { cStore.set({ sel: id }); main.current?.scrollTo({ top: 0 }) }

  const sorted = useMemo(() => [...games].sort(sortAlpha), [games])
  const ql = q.trim().toLowerCase()
  const shown = useMemo(() => sorted.filter((g) => passes(g, ql, filter)), [sorted, ql, filter])
  const counts: [Filter, number][] = useMemo(() => [
    ['All', games.length], ['Installed', games.filter((g) => g.installed).length], ['Played', games.filter((g) => g.playtime_min).length],
    ['Never played', games.filter((g) => !g.playtime_min).length], ['Epic', games.filter((g) => g.store === 'epic').length],
  ], [games])
  const selGame = sel ? games.find((g) => g.id === sel) : undefined

  // Keyboard: arrows move through the list, letters go to search, Enter plays (like the mockup).
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const inSearch = document.activeElement === search.current
      if (inSearch && e.key === 'Escape') { cStore.set({ q: '' }); search.current?.blur(); return }
      if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
        e.preventDefault()
        const rows = [...document.querySelectorAll<HTMLElement>('.clist .crow')].map((r) => r.dataset.c!)
        let i = rows.findIndex((id) => id === cStore.get().sel)
        i = Math.max(0, Math.min(rows.length - 1, i + (e.key === 'ArrowDown' ? 1 : -1)))
        if (rows[i]) { pick(rows[i]); requestAnimationFrame(() => document.querySelector('.clist .crow.on')?.scrollIntoView({ block: 'nearest' })) }
        return
      }
      if (inSearch) return
      const cur = cStore.get().sel
      const g = gameById(cur)
      if (e.key === 'Enter' && g?.installed) { e.preventDefault(); select(g.id); play(g.id); return }
      if (e.key.length === 1 && /[a-z0-9]/i.test(e.key) && !e.ctrlKey && !e.metaKey && !e.altKey) {
        e.preventDefault()
        search.current?.focus()
        cStore.set((s) => ({ q: s.q + e.key }))
      }
    }
    document.addEventListener('keydown', onKey)
    return () => document.removeEventListener('keydown', onKey)
  }, [])

  return (
    <div className="coll">
      <aside className="cside">
        <label className="csearch">
          <SearchIcon size={15} />
          <input ref={search} value={q} placeholder="Search your collection" onChange={(e) => cStore.set({ q: e.target.value })} />
        </label>
        <div className="cfil">
          {counts.map(([k, n]) => (
            <button key={k} className={filter === k ? 'on' : ''} onClick={() => cStore.set({ filter: k })}>{k} {n}</button>
          ))}
        </div>
        <div className={`crow home${sel === null ? ' on' : ''}`} onClick={() => pick(null)}><HomeIcon />Overview</div>
        <div className="clist" ref={list}>
          {ql ? <Group id="res" title="Results" list={shown} sel={sel} onPick={pick} /> : (
            <>
              <Group id="fav" title="Favourites" list={shown.filter((g) => g.favourite)} sel={sel} onPick={pick} />
              <Group id="inst" title="Installed" list={shown.filter((g) => g.installed)} sel={sel} onPick={pick} />
              <Group id="all" title="All games" list={shown} sel={sel} onPick={pick} />
            </>
          )}
        </div>
      </aside>
      <main className="cmain" ref={main}>
        {selGame ? <GamePage g={selGame} /> : <Overview games={games} onPick={pick} />}
      </main>
    </div>
  )
}
