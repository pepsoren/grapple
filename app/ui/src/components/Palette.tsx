import { useEffect, useMemo, useRef, useState } from 'react'
import { ago, fuzzy, hoursLong, storeLabel } from '../lib/format'
import { byRecent, select, useGames } from '../lib/games'
import { Cover } from './Art'
import { Hint } from './PadGlyph'
import { SearchIcon } from './Icons'
import type { Game } from '../lib/types'

interface Row {
  g: Game
  marks: number[]
  score: number
}

function Name({ name, marks }: { name: string; marks: number[] }) {
  if (!marks.length) return <>{name}</>
  const set = new Set(marks)
  return <>{[...name].map((c, i) => (set.has(i) ? <u key={i}>{c}</u> : c))}</>
}

/** Type to find: fuzzy over the installed games; Enter plays, Tab opens details, Esc closes. */
export function Palette({
  initial, onPlay, onDetails, onClose,
}: {
  initial: string
  onPlay: (id: string) => void
  onDetails: (id: string) => void
  onClose: () => void
}) {
  const games = useGames((s) => s.games)
  const [q, setQ] = useState(initial)
  const [k, setK] = useState(0)
  const input = useRef<HTMLInputElement>(null)
  const mouse = useRef<string | null>(null)
  useEffect(() => {
    setQ(initial)
    setK(0)
    const el = input.current
    if (el) {
      el.focus()
      const n = initial.length
      requestAnimationFrame(() => el.setSelectionRange(n, n))
    }
  }, [initial])

  const rows: Row[] = useMemo(() => {
    const query = q.trim()
    return games
      .filter((g) => g.installed)
      .map((g) => ({ g, ...fuzzy(g.name, query) }))
      .filter((r) => r.ok)
      .sort((a, b) => b.score - a.score || byRecent(a.g, b.g))
  }, [games, q])
  const sel = Math.min(k, Math.max(0, rows.length - 1))
  useEffect(() => {
    document.querySelector('.prow.on')?.scrollIntoView({ block: 'nearest' })
  }, [sel, q])

  const onKey = (e: React.KeyboardEvent) => {
    if (e.key === 'Escape') { e.preventDefault(); onClose() }
    else if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault()
      setK((sel + (e.key === 'ArrowDown' ? 1 : -1) + rows.length) % Math.max(1, rows.length))
    } else if (e.key === 'Enter' && rows[sel]) { e.preventDefault(); onPlay(rows[sel].g.id) }
    else if (e.key === 'Tab' && rows[sel]) { e.preventDefault(); onDetails(rows[sel].g.id) }
  }

  const pal = (
    <div className="pal" onClick={(e) => e.stopPropagation()}>
      <div className="inp">
        <SearchIcon size={20} />
        <input
          ref={input}
          value={q}
          placeholder="Find a game"
          autoComplete="off"
          spellCheck={false}
          onChange={(e) => { setQ(e.target.value); setK(0) }}
          onKeyDown={onKey}
        />
        <span className="sub">{rows.length} game{rows.length === 1 ? '' : 's'}</span>
      </div>
      <div className="list">
        {rows.map((r, i) => (
          <div
            key={r.g.id}
            className={`prow${i === sel ? ' on' : ''}`}
            onMouseMove={(e) => {
              // Only a real pointer move selects: a window that opens under a still pointer must not.
              const at = `${e.screenX},${e.screenY}`
              if (mouse.current !== null && mouse.current !== at && i !== sel) setK(i)
              mouse.current = at
            }}
            onClick={() => { select(r.g.id); onPlay(r.g.id) }}
          >
            <Cover g={r.g} />
            <span className="nm"><Name name={r.g.name} marks={r.marks} /></span>
            <span className="meta">{storeLabel(r.g)} · {r.g.playtime_min ? hoursLong(r.g.playtime_min) : 'New'}<br />{ago(r.g.last_played)}</span>
            {i === sel && <Hint action="confirm" keyText="↵" />}
          </div>
        ))}
        {rows.length === 0 && <div className="sub none">No game matches</div>}
      </div>
      <div className="hints">
        <span><Hint action="confirm" keyText="↵" /> Play</span>
        <span><Hint action="alt" keyText="Tab" /> Details</span>
        <span><Hint action="back" keyText="Esc" /> Close</span>
      </div>
    </div>
  )
  return <div className="scrim" onClick={onClose}>{pal}</div>
}
