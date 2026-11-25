import { artUrl } from '../lib/ipc'
import type { ArtKind, Game } from '../lib/types'

type G = Pick<Game, 'id' | 'art' | 'art_v'>

export const url = (g: G, kind: ArtKind) => (g.art[kind] ? artUrl(g, kind) : null)

/** Cover with a flat fallback. `lazy` for long lists. */
export function Cover({ g, lazy, alt = '' }: { g: G; lazy?: boolean; alt?: string }) {
  const u = url(g, 'cover')
  return u ? <img src={u} alt={alt} loading={lazy ? 'lazy' : undefined} decoding="async" draggable={false} /> : null
}

export function Icon({ g }: { g: G }) {
  const u = url(g, 'icon') ?? url(g, 'cover')
  return u ? <img src={u} alt="" loading="lazy" decoding="async" draggable={false} /> : <span className="ph" />
}

/** The wide image to put behind things: hero if there is one, else the cover (blurred by the caller). */
export function backdrop(g: G): { url: string | null; blur: boolean } {
  const h = url(g, 'hero')
  if (h) return { url: h, blur: false }
  return { url: url(g, 'cover'), blur: true }
}
