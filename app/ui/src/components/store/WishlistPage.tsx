// The wishlist (mockups/05-store.html, screen D), reached from the store bar's pill.
import { useEffect, useMemo, useState } from 'react'
import { removeFromWishlist, startStore, toggleNotify, useStoreData } from '../../lib/store-data'
import { bestDiscount, priceOn } from '../../lib/store-format'
import { storeBack } from '../../lib/store-nav'
import type { StoreGame, StoreKind } from '../../lib/store-types'
import { Img, PriceTag, headerArt, openProps } from './parts'

type Filter = 'all' | 'sale' | 'released' | 'soon'

function Cell({ g, store }: { g: StoreGame; store: StoreKind }) {
  const p = priceOn(g, store)
  return (
    <div className="st-wp">
      <span className="st-lbl">{store === 'steam' ? 'Steam' : 'Epic'}</span>
      {p ? <span className="st-pr"><PriceTag p={p} /></span>
        : store === 'steam' && g.comingSoon ? <span className="st-soon">Coming soon</span>
          : <span className="st-na">—</span>}
    </div>
  )
}

export function WishlistPage() {
  const list = useStoreData((s) => s.wishlist)
  const [filter, setFilter] = useState<Filter>('all')
  useEffect(() => startStore(), [])
  const shown = useMemo(() => {
    const f: Record<Filter, (g: StoreGame) => boolean> = {
      all: () => true, sale: (g) => bestDiscount(g) > 0, released: (g) => !g.comingSoon, soon: (g) => g.comingSoon,
    }
    return list.filter(f[filter])
  }, [list, filter])
  const n = (f: (g: StoreGame) => boolean) => list.filter(f).length
  const tabs: [Filter, string, number][] = [
    ['all', 'All', list.length],
    ['sale', 'On sale', n((g) => bestDiscount(g) > 0)],
    ['released', 'Released', n((g) => !g.comingSoon)],
    ['soon', 'Coming soon', n((g) => g.comingSoon)],
  ]
  return (
    <>
      <div className="st-whead">
        <button className="st-crumb" onClick={storeBack}>‹ Store</button>
        <h2>Wishlist</h2><span className="st-count">{list.length} games</span>
      </div>
      <div className="st-filters">
        {tabs.map(([k, label, c]) => <button key={k} className={`tab${filter === k ? ' on' : ''}`} onClick={() => setFilter(k)}>{label} {c}</button>)}
      </div>
      <div className="st-wlist">
        {shown.map((g) => {
          const d = bestDiscount(g)
          return (
            <div className="st-wrow" key={g.key} {...openProps(g.key)}>
              <div className="st-wimg"><Img src={headerArt(g.art)} /></div>
              <div className="st-wn"><b>{g.name}</b><span>{g.genres.join(', ')}</span></div>
              <Cell g={g} store="steam" />
              <Cell g={g} store="epic" />
              {g.comingSoon ? <span className="st-badge soon">Not released</span>
                : d > 0 ? <span className="st-badge sale">-{d}%</span>
                  : <span className="st-badge">Full price</span>}
              <div className="st-tell" onClick={(e) => e.stopPropagation()}>
                <button className={`tog${g.notifyOnSale ? ' on' : ''}`} role="switch" aria-checked={g.notifyOnSale} onClick={() => void toggleNotify(g)} />
                Tell me on sale
              </div>
              <button
                className="st-wrm"
                aria-label={`Remove ${g.name}`}
                title="Remove"
                onClick={(e) => { e.stopPropagation(); void removeFromWishlist(g.key) }}
                onKeyDown={(e) => e.stopPropagation()}
              >
                <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.4" strokeLinecap="round" aria-hidden><path d="M6 6l12 12M18 6L6 18" /></svg>
              </button>
            </div>
          )
        })}
      </div>
    </>
  )
}
