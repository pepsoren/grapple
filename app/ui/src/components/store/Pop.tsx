import { useEffect, useRef, useState, type ReactNode } from 'react'

/** A pill that opens a small menu under it. Closes on an outside press, Escape, or a pick. */
export function Pop({
  label, on, className = 'st-pill', right, children,
}: { label: ReactNode; on?: boolean; className?: string; right?: boolean; children: (close: () => void) => ReactNode }) {
  const [open, setOpen] = useState(false)
  const box = useRef<HTMLDivElement>(null)
  useEffect(() => {
    if (!open) return
    const down = (e: PointerEvent) => { if (!box.current?.contains(e.target as Node)) setOpen(false) }
    const key = (e: KeyboardEvent) => { if (e.key === 'Escape') { e.stopPropagation(); setOpen(false) } }
    document.addEventListener('pointerdown', down, true)
    document.addEventListener('keydown', key, true)
    return () => {
      document.removeEventListener('pointerdown', down, true)
      document.removeEventListener('keydown', key, true)
    }
  }, [open])
  return (
    <div className="st-pop" ref={box}>
      <button className={`${className}${on ? ' on' : ''}`} aria-haspopup="menu" aria-expanded={open} onClick={() => setOpen((o) => !o)}>{label}</button>
      {open && <div className={`st-menu${right ? ' right' : ''}`} role="menu">{children(() => setOpen(false))}</div>}
    </div>
  )
}
