// Small pieces every store page shares: art, prices, the "Like" stack, open-a-game props.
import type { KeyboardEvent } from 'react'
import { advantage, leadPrice, money } from '../../lib/store-format'
import { openStoreGame } from '../../lib/store-nav'
import type { Similar, StoreArt, StoreGame, StorePrice } from '../../lib/store-types'
import '../../styles/store.css'

export const coverArt = (a: StoreArt) => a.cover ?? a.header ?? a.hero
export const headerArt = (a: StoreArt) => a.header ?? a.cover ?? a.hero
export const heroArt = (a: StoreArt) => a.hero ?? a.header ?? a.cover

/** Official art, lazy and off the main thread. A missing file just leaves the placeholder fill. */
export function Img({ src, eager, alt = '' }: { src?: string; eager?: boolean; alt?: string }) {
  if (!src) return null
  return (
    <img
      src={src}
      alt={alt}
      loading={eager ? undefined : 'lazy'}
      decoding="async"
      draggable={false}
      onError={(e) => { e.currentTarget.style.visibility = 'hidden' }}
    />
  )
}

/** Click + Enter to open a game's page. */
export function openProps(key: string) {
  return {
    role: 'button' as const,
    tabIndex: 0,
    onClick: () => openStoreGame(key),
    onKeyDown: (e: KeyboardEvent) => { if (e.key === 'Enter') openStoreGame(key) },
  }
}

/** One store's price: "-40% R 185.00 R 111.00", or just "R 699.00". */
export function PriceTag({ p }: { p: StorePrice }) {
  if (p.discountPct > 0) {
    return (
      <>
        <span className="st-disc">-{p.discountPct}%</span>
        <span className="st-was">{money(p.original)}</span>
        <span className="st-now">{money(p.final)}</span>
      </>
    )
  }
  return <span className="st-now">{money(p.final)}</span>
}

/** What a card leads with: "Free", "Coming soon", the cheapest store's price, or a dash when no store has one. */
export function Price({ g }: { g: StoreGame }) {
  if (g.free) return <span className="st-now">Free</span>
  if (g.comingSoon) return <span className="st-soon">Coming soon</span>
  const p = leadPrice(g)
  return p ? <PriceTag p={p} /> : <span className="st-na">—</span>
}

/** Green is only ever a price advantage. */
export function Cheaper({ g }: { g: StoreGame }) {
  const a = advantage(g)
  if (!a) return null
  return <span className={a.better ? 'st-cheap' : 'st-same'}>{a.text}</span>
}

const FRIEND_TONES = ['var(--avatar-1)', 'var(--avatar-4)', 'var(--avatar-2)', 'var(--avatar-3)', 'var(--avatar-5)']

function Stack({ items }: { items: Similar[] }) {
  return (
    <span className="st-stack">
      {items.map((s, i) =>
        s.kind === 'game' && s.icon
          ? <img key={s.id} src={s.icon} alt="" title={s.name} loading="lazy" decoding="async" draggable={false} />
          : <span key={s.id} className="st-fav" title={s.name} style={{ background: FRIEND_TONES[i % FRIEND_TONES.length] }}>{s.name[0]?.toUpperCase()}</span>,
      )}
    </span>
  )
}

/** Why it is picked: small icons of your own games (or the friends who play it), nothing else. No text, no
 *  playtime; the names are in each icon's tooltip. */
export function Like({ similar }: { similar: Similar[] }) {
  const games = similar.filter((s) => s.kind === 'game')
  const friends = similar.filter((s) => s.kind === 'friend')
  if (!games.length && !friends.length) return null
  return (
    <>
      {games.length > 0 && <div className="st-like" role="img" aria-label={`Like ${games.map((g) => g.name).join(', ')}`}><Stack items={games} /></div>}
      {friends.length > 0 && <div className="st-like" role="img" aria-label={`${friends.map((f) => f.name).join(', ')} play this`}><Stack items={friends} /></div>}
    </>
  )
}

export const Heart = ({ on }: { on: boolean }) => (
  <svg width="1em" height="1em" viewBox="0 0 24 24" fill={on ? 'currentColor' : 'none'} stroke="currentColor" strokeWidth="2" strokeLinejoin="round" aria-hidden>
    <path d="M12 21.35l-1.45-1.32C5.4 15.36 2 12.28 2 8.5 2 5.42 4.42 3 7.5 3c1.74 0 3.41.81 4.5 2.09C13.09 3.81 14.76 3 16.5 3 19.58 3 22 5.42 22 8.5c0 3.78-3.4 6.86-8.55 11.54z" />
  </svg>
)

/** A 2:3 cover tile with title, price, optional "Like" and an extra line. */
export function Card({ g, extra }: { g: StoreGame; extra?: string }) {
  return (
    <div className="st-card" {...openProps(g.key)}>
      <div className="st-cv">
        <Img src={coverArt(g.art)} />
        {g.wishlisted && <span className="st-wl"><Heart on /></span>}
      </div>
      <div className="st-t">{g.name}</div>
      <div className="st-pr"><Price g={g} /></div>
      <Like similar={g.similar} />
      {extra && <div className="st-rel">{extra}</div>}
    </div>
  )
}
