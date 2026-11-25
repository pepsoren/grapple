import { useState, type ReactNode } from 'react'
import type { Friend } from '../tether'

type Person = Pick<Friend, 'initial' | 'colour' | 'name'> & { avatar?: string | null }

/** A person's round avatar: their tether picture once Rust has cached it (grapple://avatar/...), else the
 *  initial on their colour. A picture that fails to load falls back to the initial. `children` (a status dot)
 *  sit on top. `noTitle` leaves the hover text to the parent. */
export function Avatar({ f, size, noTitle, children }: { f: Person; size?: number; noTitle?: boolean; children?: ReactNode }) {
  const [broken, setBroken] = useState<string | null>(null)
  const pic = f.avatar && f.avatar !== broken ? f.avatar : null
  return (
    <span
      className={`av${pic ? ' pic' : ''}`}
      title={noTitle ? undefined : f.name}
      style={{ background: pic ? undefined : f.colour, ...(size ? { width: size, height: size, fontSize: Math.round(size * 0.42) } : null) }}
    >
      {pic ? <img className="av-img" src={pic} alt="" draggable={false} onError={() => setBroken(pic)} /> : f.initial}
      {children}
    </span>
  )
}
