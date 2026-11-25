// Home's "Games we think you'd like" (mockups/06-store.html, screen B): your five strongest matches.
import { useEffect } from 'react'
import { loadHomePicks, useStoreData } from '../../lib/store-data'
import { openStore, openStoreGame } from '../../lib/store-nav'
import { Img, Like, Price, headerArt } from './parts'

export function HomePicks() {
  const picks = useStoreData((s) => s.homePicks)
  useEffect(() => { void loadHomePicks() }, [])
  if (!picks.length) return null
  return (
    <section className="st-homepicks">
      <div className="st-rh">
        <h3>Games we think you'd like</h3>
        <button className="st-btn" onClick={openStore}>Browse the store</button>
      </div>
      <div className="st-hp-grid">
        {picks.slice(0, 5).map((g) => (
          <div className="st-hp" key={g.key} role="button" tabIndex={0} onClick={() => openStoreGame(g.key)} onKeyDown={(e) => { if (e.key === 'Enter') openStoreGame(g.key) }}>
            <div className="st-himg"><Img src={headerArt(g.art)} /></div>
            <div className="st-hpi">
              <b>{g.name}</b>
              <Like similar={g.similar} />
              <div className="st-pr"><Price g={g} /></div>
            </div>
          </div>
        ))}
      </div>
    </section>
  )
}
