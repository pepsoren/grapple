// Small pieces shared by the downloads screens (mockups/07-downloads.html).
import { useEffect, useRef, useState, type ReactNode } from 'react'
import { createPortal } from 'react-dom'
import type { DlItem } from '../../lib/download-types'

export const StoreBadge = ({ store }: { store: DlItem['store'] }) => <span className="dl-badge">{store === 'epic' ? 'Epic' : 'Steam'}</span>

export const Bar = ({ value, warn, className = '' }: { value: number; warn?: boolean; className?: string }) => (
  <div className={`dl-bar${warn ? ' warn' : ''} ${className}`}><i style={{ width: `${Math.round(Math.max(0, Math.min(1, value)) * 1000) / 10}%` }} /></div>
)

/** The speed sparkline: the last 20 samples, scaled to the fastest. */
export function Spark({ history }: { history: number[] }) {
  const max = Math.max(1, ...history)
  return (
    <div className="dl-spark" aria-hidden>
      {history.map((v, i) => <i key={i} style={{ height: `${Math.max(2, Math.round((v / max) * 34))}px` }} />)}
    </div>
  )
}

/** A pill that opens a small menu. Closes on an outside press, Escape, or a pick. */
export function Pick({ label, down, children }: { label: ReactNode; down?: boolean; children: (close: () => void) => ReactNode }) {
  const [open, setOpen] = useState(false)
  const box = useRef<HTMLDivElement>(null)
  useEffect(() => {
    if (!open) return
    const press = (e: PointerEvent) => { if (!box.current?.contains(e.target as Node)) setOpen(false) }
    const key = (e: KeyboardEvent) => { if (e.key === 'Escape') { e.stopPropagation(); setOpen(false) } }
    document.addEventListener('pointerdown', press, true)
    document.addEventListener('keydown', key, true)
    return () => {
      document.removeEventListener('pointerdown', press, true)
      document.removeEventListener('keydown', key, true)
    }
  }, [open])
  return (
    <div className="dl-pick" ref={box}>
      <button className="dl-pill" aria-haspopup="menu" aria-expanded={open} onClick={() => setOpen((o) => !o)}>{label} ▾</button>
      {open && <div className={`dl-menu${down ? ' down' : ''}`} role="menu">{children(() => setOpen(false))}</div>}
    </div>
  )
}

/** A yes/no question over the whole window (portalled, so it never scrolls with a page). */
export function Confirm({ title, text, yes, onYes, onNo }: { title: string; text?: string; yes: string; onYes: () => void; onNo: () => void }) {
  useEffect(() => {
    const key = (e: KeyboardEvent) => { if (e.key === 'Escape') { e.stopPropagation(); onNo() } }
    document.addEventListener('keydown', key, true)
    return () => document.removeEventListener('keydown', key, true)
  }, [onNo])
  return createPortal(
    <div className="dl-scrim" onClick={onNo}>
      <div className="dl-confirm" role="dialog" aria-modal onClick={(e) => e.stopPropagation()}>
        <b>{title}</b>
        {text && <p>{text}</p>}
        <div className="dl-acts">
          <button className="dl-btn dl-sec" onClick={onNo}>Cancel</button>
          <button className="dl-btn dl-play" autoFocus onClick={onYes}>{yes}</button>
        </div>
      </div>
    </div>,
    document.body,
  )
}
