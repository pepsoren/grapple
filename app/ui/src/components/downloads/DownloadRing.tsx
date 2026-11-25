// B · The top bar's download ring and its popover (mockups/07-downloads.html). Shown while anything is active or
// queued; the count is active + queued.
import { useEffect, useRef, useState } from 'react'
import type { DlItem } from '../../lib/download-types'
import { left, ofTotal, overall, progress, size, speed, STATE_WORD, useDownloads } from '../../lib/downloads'
import { openDownloads } from '../../lib/ui'
import { DownloadIcon } from '../Icons'
import { Bar } from './parts'
import '../../styles/downloads.css'

const R = 15
const C = 2 * Math.PI * R

function PopRow({ item, active }: { item: DlItem; active: boolean }) {
  const img = item.art.cover ?? item.art.header ?? item.art.hero
  return (
    <div className="dl-pr">
      {img ? <img src={img} alt="" /> : <span />}
      <div>
        <b>{item.name}</b>
        {active ? (
          <>
            <div className="sub"><span>{ofTotal(item.bytesDone, item.bytesTotal)} · {speed(item.bytesPerSec)}</span><span>{left(item.secondsLeft)}</span></div>
            <Bar value={progress(item)} />
          </>
        ) : (
          <div className="sub"><span>{STATE_WORD[item.state]} · {size(item.bytesTotal)}</span></div>
        )}
      </div>
    </div>
  )
}

export function DownloadRing() {
  const snap = useDownloads((s) => s.snap)
  const [open, setOpen] = useState(false)
  const box = useRef<HTMLDivElement>(null)
  useEffect(() => {
    if (!open) return
    const press = (e: PointerEvent) => { if (!box.current?.contains(e.target as Node)) setOpen(false) }
    const key = (e: KeyboardEvent) => { if (e.key === 'Escape') setOpen(false) }
    document.addEventListener('pointerdown', press, true)
    document.addEventListener('keydown', key)
    return () => { document.removeEventListener('pointerdown', press, true); document.removeEventListener('keydown', key) }
  }, [open])
  if (!snap || (!snap.active && !snap.queued.length)) return null
  const count = (snap.active ? 1 : 0) + snap.queued.length
  const list = [snap.active, ...snap.queued].filter((x): x is DlItem => !!x)
  const more = list.length - 3
  const ups = snap.updates.length
  const foot = [more > 0 ? `+${more} more` : '', ups ? `${ups} ${ups === 1 ? 'update' : 'updates'}` : ''].filter(Boolean).join(' · ')
  return (
    <div ref={box} style={{ display: 'contents' }}>
      <button className={`dl-ring${open ? ' on' : ''}`} title="Downloads" onClick={() => setOpen((o) => !o)}>
        <svg className="ring" width="32" height="32" viewBox="0 0 36 36">
          <circle className="track" cx="18" cy="18" r={R} fill="none" strokeWidth="3" />
          <circle className="fill" cx="18" cy="18" r={R} fill="none" strokeWidth="3" strokeLinecap="round" strokeDasharray={C} strokeDashoffset={C * (1 - overall(snap))} transform="rotate(-90 18 18)" />
        </svg>
        <DownloadIcon size={16} />
        <span className="cnt">{count}</span>
      </button>
      {open && (
        <div className="dl-pop">
          {list.slice(0, 3).map((i) => <PopRow key={i.game} item={i} active={i === snap.active} />)}
          <div className="foot">
            <span className="sub">{foot}</span>
            <button className="dl-btn dl-sec dl-sm" onClick={() => { setOpen(false); openDownloads() }}>Open downloads</button>
          </div>
        </div>
      )}
    </div>
  )
}
