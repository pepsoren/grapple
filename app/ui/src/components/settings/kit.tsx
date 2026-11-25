// The pieces every Settings section is built from (mockups/08-settings.html): groups, cards, rows, switches,
// segmented controls and pills. Rows know their label, so "Find a setting" can leave out the ones that do not match.
import { createContext, useContext, useEffect, type ReactNode } from 'react'
import { createPortal } from 'react-dom'
import { CloseIcon } from '../Icons'
import { Pop } from '../store/Pop'

/** The "Find a setting" text (lower case, trimmed); empty when not searching. */
export const FindQuery = createContext('')

export function matches(text: string, q: string): boolean {
  if (!q) return true
  const t = text.toLowerCase()
  return q.split(/\s+/).every((w) => t.includes(w))
}

/** Children only when `label` matches the search. */
export function Match({ label, children }: { label: string; children: ReactNode }) {
  const q = useContext(FindQuery)
  return matches(label, q) ? <>{children}</> : null
}

export function Pane({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="set-pane">
      <h3>{title}</h3>
      {children}
    </section>
  )
}

/** A group heading and its card. `os` tags rows only one system has ("Linux"). */
export function Group({ title, os, children, className = '' }: { title?: string; os?: string; children: ReactNode; className?: string }) {
  return (
    <>
      {title && <div className="set-grp">{title}{os && <span className="os">{os}</span>}</div>}
      <div className={`set-card ${className}`}>{children}</div>
    </>
  )
}

export function Row({ label, sub, children, sub1, keywords, mono }: {
  label: ReactNode
  sub?: ReactNode
  children?: ReactNode
  /** Indented under the row above (it only matters when that one is on). */
  sub1?: boolean
  /** Extra words "Find a setting" matches. */
  keywords?: string
  /** The sub-label is a path. */
  mono?: boolean
}) {
  const q = useContext(FindQuery)
  const text = `${typeof label === 'string' ? label : ''} ${typeof sub === 'string' ? sub : ''} ${keywords ?? ''}`
  if (!matches(text, q)) return null
  return (
    <div className={`set-row${sub1 ? ' sub1' : ''}`}>
      <div className="l">
        <b>{label}</b>
        {sub && <span className={mono ? 'set-mono' : undefined}>{sub}</span>}
      </div>
      {children !== undefined && <div className="r">{children}</div>}
    </div>
  )
}

export function Switch({ on, onChange, disabled, label }: { on: boolean; onChange: (v: boolean) => void; disabled?: boolean; label?: string }) {
  return <button className={`tog${on ? ' on' : ''}`} role="switch" aria-checked={on} aria-label={label} disabled={disabled} onClick={() => onChange(!on)} />
}

export function Seg<T extends string | number>({ value, options, onChange }: { value: T; options: [T, string][]; onChange: (v: T) => void }) {
  return (
    <div className="set-seg" role="radiogroup">
      {options.map(([v, label]) => (
        <button key={String(v)} role="radio" aria-checked={v === value} className={v === value ? 'on' : ''} onClick={() => onChange(v)}>{label}</button>
      ))}
    </div>
  )
}

/** A pill that opens a short list (store/Pop.tsx). */
export function Choice<T extends string | number | boolean | null>({ value, options, onChange, mono, label }: {
  value: T
  options: [T, string][]
  onChange: (v: T) => void
  mono?: boolean
  /** The pill's text when it should differ from the chosen option's. */
  label?: string
}) {
  const cur = options.find(([v]) => v === value)
  return (
    <Pop label={<>{label ?? cur?.[1] ?? '—'} ▾</>} className={`set-pill${mono ? ' mono' : ''}`} right>
      {(close) => options.map(([v, text]) => (
        <button key={String(v)} className={v === value ? 'on' : ''} onClick={() => { onChange(v); close() }}>
          {text}{v === value && <i>✓</i>}
        </button>
      ))}
    </Pop>
  )
}

export function Btn({ children, onClick, danger, disabled, title }: { children: ReactNode; onClick: () => void; danger?: boolean; disabled?: boolean; title?: string }) {
  return <button className={`set-btn${danger ? ' danger' : ''}`} disabled={disabled} title={title} onClick={onClick}>{children}</button>
}

/** "32 MB", "5.5 MB", "552 GB". */
export function bytes(n: number | null | undefined): string {
  if (n == null) return '—'
  if (n >= 100e9) return `${Math.round(n / 1e9)} GB`
  if (n >= 1e9) return `${parseFloat((n / 1e9).toFixed(1))} GB`
  if (n >= 10e6) return `${Math.round(n / 1e6)} MB`
  if (n >= 1e6) return `${parseFloat((n / 1e6).toFixed(1))} MB`
  if (n > 0) return `${Math.max(1, Math.round(n / 1e3))} KB`
  return '0 MB'
}

/** "2 min ago", "3 h ago", "Yesterday". */
export function agoShort(t: number | null | undefined): string {
  if (!t) return 'Never'
  const s = Math.max(0, Math.floor(Date.now() / 1000) - t)
  if (s < 60) return 'Just now'
  if (s < 3600) return `${Math.floor(s / 60)} min ago`
  if (s < 86400) return `${Math.floor(s / 3600)} h ago`
  if (s < 2 * 86400) return 'Yesterday'
  return `${Math.floor(s / 86400)} days ago`
}

/** Copy text to the clipboard (lib/clipboard.ts; invites use it too). */
export { copyText } from '../../lib/clipboard'

/** A small window over the page with a title and a list (hidden games, games I don't share). Esc closes it. */
export function Dialog({ title, onClose, children }: { title: string; onClose: () => void; children: ReactNode }) {
  useEffect(() => {
    const key = (e: KeyboardEvent) => { if (e.key === 'Escape') { e.stopPropagation(); onClose() } }
    document.addEventListener('keydown', key, true)
    return () => document.removeEventListener('keydown', key, true)
  }, [onClose])
  return createPortal(
    <div className="set-scrim" onClick={onClose}>
      <div className="set-dialog" role="dialog" aria-modal aria-label={title} onClick={(e) => e.stopPropagation()}>
        <div className="hd"><b>{title}</b><button className="set-x" title="Close (Esc)" onClick={onClose}><CloseIcon /></button></div>
        <div className="bd">{children}</div>
      </div>
    </div>,
    document.body,
  )
}
