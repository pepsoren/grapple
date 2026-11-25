import { useEffect, useState } from 'react'
import { clock } from '../lib/format'
import { api } from '../lib/ipc'
import { useGames } from '../lib/games'
import { dismissFailure, expand, stop, useSession } from '../lib/session'
import { useSettings } from '../lib/settings'
import { useTether } from '../tether'
import { Cover, backdrop } from './Art'

/** Steps while the game starts (from the launcher's events), or the reason it failed. */
export function LaunchCard() {
  const phase = useSession((s) => s.phase)
  const id = useSession((s) => s.game)
  const steps = useSession((s) => s.steps)
  const message = useSession((s) => s.message)
  const g = useGames((s) => s.all.find((x) => x.id === id))
  if (!g || (phase !== 'launching' && phase !== 'failed')) return null
  const bd = backdrop(g)
  const shown = steps.length ? steps : [{ label: 'Getting ready', done: false }]
  const done = shown.filter((s) => s.done).length
  const pct = phase === 'failed' ? 100 : Math.min(100, Math.round(((done + 0.5) / Math.max(3, shown.length)) * 100))
  return (
    <div className="launch">
      <div className={`lcard${phase === 'failed' ? ' failed' : ''}`}>
        <div className="art" style={bd.url ? { backgroundImage: `url("${bd.url}")` } : undefined} />
        <div className="in">
          <div className="name">{g.name}</div>
          {phase === 'failed' ? (
            <>
              <div className="msg">{message}</div>
              <button className="btn play neutral" style={{ height: 36, fontSize: 14, padding: '0 16px' }} onClick={dismissFailure}>Close</button>
            </>
          ) : (
            <>
              <div className="steps">
                {shown.map((s, i) => (
                  <div key={s.label + i} className={`step ${s.done ? 'done' : 'now'}`}>
                    <span className="ic">{s.done ? '✓' : '●'}</span>{s.label}
                  </div>
                ))}
              </div>
              <div className="pbar"><div style={{ transform: `scaleX(${pct / 100})` }} /></div>
            </>
          )}
        </div>
      </div>
    </div>
  )
}

function useClock(startedAt: number | null) {
  const [now, setNow] = useState(() => Math.floor(Date.now() / 1000))
  useEffect(() => {
    const t = setInterval(() => setNow(Math.floor(Date.now() / 1000)), 1000)
    return () => clearInterval(t)
  }, [])
  return startedAt ? now - startedAt : 0
}

/** The 380x92 window: cover, live timer, Stop. */
export function PlayingCard() {
  const id = useSession((s) => s.game)
  const startedAt = useSession((s) => s.startedAt)
  const g = useGames((s) => s.all.find((x) => x.id === id))
  const signedIn = useTether((s) => s.signedIn)
  const shareOn = useSettings((s) => s.show_in_tether)
  const [gameShare, setGameShare] = useState(true)
  useEffect(() => {
    if (id) api.getGameSettings(id).then((gs) => setGameShare(gs.show_in_tether), () => setGameShare(true))
  }, [id])
  const tetherOn = signedIn && shareOn && gameShare
  const secs = useClock(startedAt)
  if (!g) return null
  return (
    <div className="playing" data-tauri-drag-region>
      <Cover g={g} />
      <div className="mid" data-tauri-drag-region>
        <div className="nm" data-tauri-drag-region>{g.name}</div>
        <div className="clock" data-tauri-drag-region>{clock(secs)}</div>
        {tetherOn && <div className="sub" data-tauri-drag-region><i />Shown in tether</div>}
      </div>
      <button className="btn stop" onClick={stop}>Stop</button>
    </div>
  )
}

/** Playing, but the window is the full one (Esc on the card, or a game found running): a small pill. */
export function NowPlaying() {
  const id = useSession((s) => s.game)
  const startedAt = useSession((s) => s.startedAt)
  const g = useGames((s) => s.all.find((x) => x.id === id))
  const secs = useClock(startedAt)
  if (!g) return null
  return (
    <div className="nowplaying glass">
      <span>Playing <b>{g.name}</b></span>
      <span className="t">{clock(secs)}</span>
      <button className="btn stop pill-btn" style={{ background: 'var(--surface-control)', color: 'var(--text-strong)' }} onClick={stop}>Stop</button>
    </div>
  )
}

export { expand }
