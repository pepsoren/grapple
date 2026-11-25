// The Store route: one scroller, three pages (front, game, wishlist) with a way back. Store settings are in
// Settings › Store.
import { useEffect, useLayoutEffect } from 'react'
import { registerStoreScroller, storeBack, storeNav, useStoreNav } from '../../lib/store-nav'
import { uiStore } from '../../lib/ui'
import { Front } from './Front'
import { GamePage } from './GamePage'
import { WishlistPage } from './WishlistPage'
import '../../styles/store-page.css'

export default function StoreRoot({ scroller }: { scroller: React.RefObject<HTMLDivElement | null> }) {
  const page = useStoreNav((s) => s.page)
  const tick = useStoreNav((s) => s.tick)

  useLayoutEffect(() => {
    registerStoreScroller(scroller.current)
    return () => registerStoreScroller(null)
  }, [scroller])

  // After a page change: the saved position on the way back, else the top.
  useLayoutEffect(() => {
    scroller.current?.scrollTo({ top: storeNav.get().y })
  }, [tick, scroller])

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== 'Escape' || uiStore.get().menu) return
      const el = document.activeElement as HTMLElement | null
      if (el && (el.tagName === 'INPUT' || el.tagName === 'TEXTAREA')) return
      if (storeNav.get().page.kind !== 'front') storeBack()
    }
    document.addEventListener('keydown', onKey)
    return () => document.removeEventListener('keydown', onKey)
  }, [])

  return (
    <div className="app st-page" ref={scroller}>
      {page.kind === 'front' && <Front />}
      {page.kind === 'game' && <GamePage gameKey={page.key} />}
      {page.kind === 'wishlist' && <WishlistPage />}
    </div>
  )
}
