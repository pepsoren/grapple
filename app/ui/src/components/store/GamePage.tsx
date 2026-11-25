// A game page (mockups/06-store.html, screen C): hero art, screenshot strip, where to buy, more like this.
import { useEffect } from 'react'
import { buy, loadGame, toggleNotify, toggleWishlist, useStoreData } from '../../lib/store-data'
import { leadPrice, money, priceOn, storeName } from '../../lib/store-format'
import { storeBack } from '../../lib/store-nav'
import type { StoreGame, StoreKey, StoreKind } from '../../lib/store-types'
import { Card, Cheaper, Heart, Img, Like, PriceTag, heroArt } from './parts'

function BuyRow({ g, store, primary }: { g: StoreGame; store: StoreKind; primary: boolean }) {
  const p = priceOn(g, store)
  if (!p) {
    return (
      <div className="st-buyrow off">
        <div className="st">{storeName(store)}</div>
        <div className="st-pr"><span className="st-na">No {store === 'epic' ? 'Epic ' : ''}price</span></div>
      </div>
    )
  }
  return (
    <div className="st-buyrow">
      <div className="st">{storeName(store)}</div>
      <div className="st-pr">{g.free ? <span className="st-now">Free</span> : <PriceTag p={p} />}</div>
      <button className={`btn ${primary ? 'play' : 'sec'}`} onClick={() => void buy(g.key, store)}>{g.free ? 'Get' : 'Buy'} on {storeName(store)}</button>
    </div>
  )
}

export function GamePage({ gameKey }: { gameKey: StoreKey }) {
  const g = useStoreData((s) => s.details[gameKey])
  const settings = useStoreData((s) => s.settings)
  useEffect(() => { void loadGame(gameKey) }, [gameKey, g])
  if (!g) return <div className="st-ghero"><div className="st-gshade" /><div className="st-gtitle"><button className="st-crumb" onClick={storeBack}>‹ Store</button></div></div>
  // A store row shows when it has a price, or is the game's own store, or (for a Steam game) is Epic: "No Epic price".
  const own: StoreKind = g.key.startsWith('epic:') ? 'epic' : 'steam'
  const stores = (['steam', 'epic'] as StoreKind[]).filter(
    (k) => priceOn(g, k) || ((k === own || (own === 'steam' && k === 'epic')) && (!settings || settings.stores[k])),
  )
  const lead = leadPrice(g)
  const primary = lead?.store ?? 'steam'
  const shots = g.art.shots.slice(0, 4)
  const meta = [g.developers.join(', '), g.release, g.reviews ? `${g.reviews.toLocaleString('en-GB')} Steam reviews` : ''].filter(Boolean).join(' · ')
  return (
    <>
      <div className="st-ghero">
        <Img src={heroArt(g.art)} eager />
        <div className="st-gshade" />
        <div className="st-gtitle">
          <button className="st-crumb" onClick={storeBack}>‹ Store</button>
          <h1>{g.name}</h1>
          <div className="st-meta">{meta}</div>
          <div className="st-tags">{g.genres.slice(0, 4).map((t) => <span key={t}>{t}</span>)}</div>
        </div>
      </div>
      <div className="st-gbody">
        <div className="st-gmain">
          {shots.length > 0 && <div className={`st-shots n${shots.length}`}>{shots.map((s) => <Img key={s} src={s} />)}</div>}
          <p className="st-desc">{g.short}</p>
          <Like similar={g.similar} />
          {g.moreLikeThis && g.moreLikeThis.length > 0 && (
            <>
              <div className="st-rh"><h3>More like this</h3></div>
              <div className="st-rl">{g.moreLikeThis.map((m) => <Card key={m.key} g={m} />)}</div>
            </>
          )}
        </div>
        <aside className="st-buy">
          <div className="h">Where to buy</div>
          {g.owned
            ? <div className="st-buyrow"><div className="st">In your library</div></div>
            : g.comingSoon
              ? <div className="st-buyrow"><div className="st"><span className="st-soon">Coming soon</span></div></div>
              : stores.map((k) => <BuyRow key={k} g={g} store={k} primary={k === primary} />)}
          {!g.free && !g.owned && !g.comingSoon && <Cheaper g={g} />}
          {g.lowestSeen && <div className="st-lowest">Lowest price grapple has seen: <b>{money(g.lowestSeen)}</b></div>}
          <button className="st-ghost wide" onClick={() => void toggleWishlist(g)}><Heart on={g.wishlisted} />{g.wishlisted ? 'On wishlist' : 'Add to wishlist'}</button>
          {g.wishlisted && (
            <div className="st-tell">
              <button className={`tog${g.notifyOnSale ? ' on' : ''}`} role="switch" aria-checked={g.notifyOnSale} onClick={() => void toggleNotify(g)} />
              Tell me on sale
            </div>
          )}
        </aside>
      </div>
    </>
  )
}
