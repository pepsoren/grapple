import { useEffect, useState } from 'react'
import { ago, hm, storeLabel } from '../lib/format'
import { loadStats, useGames } from '../lib/games'
import { api } from '../lib/ipc'
import { removeManual } from '../lib/library'
import { play } from '../lib/session'
import { openGameSheet, setDrawer, toast, useUi } from '../lib/ui'
import type { ArtKind, Game } from '../lib/types'
import { useTether } from '../tether'
import { Bars } from './Home'
import { backdrop, url } from './Art'
import { CloseIcon, PlayIcon, TetherIcon } from './Icons'

const ART: [ArtKind, string][] = [['cover', 'Cover'], ['hero', 'Background'], ['logo', 'Logo'], ['icon', 'Icon']]

/** The four pictures grapple shows for a game; click one to use your own, Reset for the store's again. */
function ArtPicker({ g }: { g: Game }) {
  const [custom, setCustom] = useState<ArtKind[]>([])
  const load = () => api.artCustom(g.id).then(setCustom, () => setCustom([]))
  useEffect(() => { void load() }, [g.id, g.art_v]) // eslint-disable-line react-hooks/exhaustive-deps
  const pick = (k: ArtKind) => api.artPick(g.id, k).then((ok) => { if (ok) void load() }, (e) => toast(String(e)))
  const reset = (k: ArtKind) => api.artReset(g.id, k).then(load, (e) => toast(String(e)))
  return (
    <div>
      <div className="h2">Art</div>
      <div className="arts">
        {ART.map(([k, label]) => {
          const u = url(g, k)
          return (
            <div key={k} className={`art ${k}`}>
              <button className="pick" title={`Change ${label.toLowerCase()}`} onClick={() => void pick(k)}>
                {u ? <img src={u} alt="" /> : <span className="none">+</span>}
              </button>
              <div className="l">
                {label}
                {custom.includes(k) && <button className="reset" onClick={() => void reset(k)}>Reset</button>}
              </div>
            </div>
          )
        })}
      </div>
    </div>
  )
}

function Body({ g }: { g: Game }) {
  const stats = useGames((s) => s.stats[g.id])
  const note = useTether((s) => (s.signedIn ? s.notes[g.id] : undefined))
  useEffect(() => { void loadStats(g.id) }, [g.id])
  const bd = backdrop(g)
  return (
    <>
      <div className="top">
        {bd.url && <img src={bd.url} alt="" style={bd.blur ? { filter: 'var(--cover-blur)' } : undefined} />}
        <div className="fade" />
        <button className="btn ghost x" title="Close (Esc)" onClick={() => setDrawer(false)}><CloseIcon /></button>
        <div className="name">{g.name}</div>
      </div>
      <div className="body">
        <button className="btn play" onClick={() => play(g.id)}><PlayIcon />Play</button>
        <div className="grid2">
          <div className="stat"><span className="sub">Total</span><b>{hm(stats?.total_min ?? g.playtime_min)}</b></div>
          <div className="stat"><span className="sub">Last played</span><b>{ago(g.last_played)}</b></div>
          <div className="stat"><span className="sub">Sessions</span><b>{stats?.sessions ?? '—'}</b></div>
          <div className="stat"><span className="sub">Longest</span><b>{stats ? hm(stats.longest_min) : '—'}</b></div>
        </div>
        <div>
          <div className="h2">Last 14 days</div>
          <Bars days={stats?.last_14_days} h={64} />
        </div>
        <div className="how">
          <div className="t">How it launches</div>
          <div className="d">
            {g.store === 'epic'
              ? 'Gets your Epic token itself and starts the game through Proton. No Epic or Heroic process at all.'
              : g.store === 'steam'
                ? 'Steam starts hidden in the background (the game needs it for DRM and online), then the game runs straight away. You never see Steam.'
                : 'grapple starts the program you chose directly. No launcher involved.'}
          </div>
          <button className="btn ghost wide" onClick={() => openGameSheet(g.id)}>Game settings</button>
        </div>
        <ArtPicker g={g} />
        {note && <div className="note"><TetherIcon className="accent" />{note}</div>}
        {g.store === 'manual' && (
          <button className="btn ghost" title="Removes it from grapple only; the program itself is not touched" onClick={() => void removeManual(g)}>Remove from grapple</button>
        )}
        <div className="sub">{storeLabel(g)}{g.size_bytes ? ` · ${(g.size_bytes / 1e9).toFixed(1)} GB` : ''}</div>
      </div>
    </>
  )
}

export function Drawer() {
  const open = useUi((s) => s.drawer)
  const g = useGames((s) => s.all.find((x) => x.id === s.selected))
  return <aside className={`drawer${open ? ' open' : ''}`}>{open && g && <Body g={g} />}</aside>
}
