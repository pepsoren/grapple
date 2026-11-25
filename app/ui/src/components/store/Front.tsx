// The store front (mockups/06-store.html, screen A): a hero that nearly fills the window, then rows that each
// lay their games out differently. Rendered from the cache first; sections refresh in the background.
import { useEffect, useMemo, useRef, useState } from 'react'
import { storeApi, type StoreFilters } from '../../lib/store-api'
import { loadTier, showHiddenNew, startStore, storeData, toggleWishlist, updateStoreSettings, useStoreData } from '../../lib/store-data'
import { advantage, bestDiscount, currencyLabel, money, priceOn, storeName, tierLabel, tiersFor } from '../../lib/store-format'
import { openStoreGame, openStoreSettings, openWishlist, storeSearch, useStoreSearch } from '../../lib/store-nav'
import type { StoreFront, StoreGame, StoreKind } from '../../lib/store-types'
import { SearchIcon } from '../Icons'
import { Pop } from './Pop'
import { Card, Cheaper, Heart, Img, Like, Price, coverArt, headerArt, heroArt, openProps } from './parts'
import '../../styles/store-page.css'

const HERO_MS = 9000
/** Off-screen rows keep their measured height (content-visibility), so the scrollbar does not jump. */
const H = (px: number) => ({ containIntrinsicSize: `auto ${px}px` })

function Hero({ games }: { games: StoreGame[] }) {
  const list = useMemo(() => games.slice(0, 5), [games])
  const [i, setI] = useState(0)
  const n = list.length
  const idx = n ? i % n : 0
  useEffect(() => {
    if (n < 2) return
    const t = setTimeout(() => setI((x) => (x + 1) % n), HERO_MS)
    return () => clearTimeout(t)
  }, [i, n])
  const g = list[idx]
  if (!g) return <section className="st-hero" />
  const stores = g.prices.map((p) => p.store)
  return (
    <section className="st-hero">
      {list.map((x, k) => (
        <div key={x.key} className={`st-slide${k === idx ? ' on' : ''}`}><Img src={heroArt(x.art)} eager={k === 0} /></div>
      ))}
      <div className="st-shade" />
      <div className="st-hinfo" key={g.key}>
        <span className="st-kicker">{g.similar.length ? 'Picked for you' : 'Featured'}</span>
        <div className="st-htitle">{g.name}</div>
        <Like similar={g.similar} />
        <div className="st-hprice">
          <Price g={g} />
          {stores.length === 1 && <span className="st-sub">· {storeName(stores[0])} only</span>}
          {stores.length > 1 && <Cheaper g={g} />}
        </div>
        <div className="st-hbtns">
          <button className="btn play" onClick={() => openStoreGame(g.key)}>View game</button>
          <button className="st-ghost" onClick={() => void toggleWishlist(g)}><Heart on={g.wishlisted} />Wishlist</button>
        </div>
      </div>
      {n > 1 && (
        <div className="st-dots">
          {list.map((x, k) => <button key={x.key} className={k === idx ? 'on' : ''} aria-label={x.name} onClick={() => setI(k)} />)}
        </div>
      )}
    </section>
  )
}

function Bar({ front, q, setQ, filters, setFilters }: {
  front: StoreFront
  q: string
  setQ: (q: string) => void
  filters: StoreFilters
  setFilters: (f: StoreFilters) => void
}) {
  const settings = useStoreData((s) => s.settings)
  const currencies = useStoreData((s) => s.currencies)
  const count = useStoreData((s) => s.wishlist.length)
  const cur = settings?.displayCurrency ?? front.under.games[0]?.prices[0]?.final.currency ?? 'USD'
  const stores = settings?.stores ?? { steam: true, epic: true }
  const storeLabel = [stores.steam && 'Steam', stores.epic && 'Epic'].filter(Boolean).join(', ') || 'None'
  const set = (patch: Partial<StoreFilters>) => setFilters({ ...filters, ...patch })
  return (
    <div className="st-bar glass">
      <div className="st-title">Store</div>
      <label className="st-search">
        <SearchIcon size={14} />
        <input
          value={q}
          onChange={(e) => setQ(e.target.value)}
          onKeyDown={(e) => { if (e.key === 'Escape') { setQ(''); e.currentTarget.blur() } }}
          placeholder="Search games, tags, developers"
          spellCheck={false}
        />
      </label>
      <Pop label={<>{filters.genre ?? 'Genres'} ▾</>} on={!!filters.genre}>
        {(close) => (
          <>
            {filters.genre && <button onClick={() => { set({ genre: undefined }); close() }}>Any genre</button>}
            {front.genres.map((g) => (
              <button key={g.name} className={filters.genre === g.name ? 'on' : ''} onClick={() => { set({ genre: g.name }); close() }}>{g.name}</button>
            ))}
          </>
        )}
      </Pop>
      <Pop label={<>Price ▾</>}>
        {(close) => (
          <>
            {tiersFor(cur).map((t) => (
              <button key={t} onClick={() => { void loadTier(t); document.getElementById('st-under')?.scrollIntoView({ block: 'start' }); close() }}>Under {tierLabel(t, cur)}</button>
            ))}
          </>
        )}
      </Pop>
      <Pop label={<>Stores: {storeLabel} ▾</>}>
        {() => (
          <>
            {(['steam', 'epic'] as StoreKind[]).map((k) => (
              <button
                key={k}
                className={stores[k] ? 'on' : ''}
                onClick={() => {
                  // At least one store stays on.
                  if (stores[k] && !stores[k === 'steam' ? 'epic' : 'steam']) return
                  void updateStoreSettings({ stores: { ...stores, [k]: !stores[k] } })
                }}
              >
                {storeName(k)}{stores[k] && <i>✓</i>}
              </button>
            ))}
          </>
        )}
      </Pop>
      <span className="st-sp" />
      <button className="st-pill" onClick={openWishlist}><Heart on />Wishlist{count > 0 && <b>{count}</b>}</button>
      <Pop label={<>{currencyLabel(cur)} ▾</>} className="st-pill cur" right>
        {(close) => (
          <>
            {currencies.map((c) => (
              <button key={c.code} className={c.code === cur ? 'on' : ''} onClick={() => { void updateStoreSettings({ displayCurrency: c.code }); close() }}>
                {c.name}{c.code === cur && <i>✓</i>}
              </button>
            ))}
            <button onClick={() => { openStoreSettings(); close() }}>Store settings</button>
          </>
        )}
      </Pop>
    </div>
  )
}

function Hidden({ n, onShow }: { n: number; onShow: () => void }) {
  if (!n) return null
  return (
    <span className="st-hid">
      {n} hidden by your filter
      <button onClick={onShow}>Show</button>
    </span>
  )
}

function Wide({ g }: { g: StoreGame }) {
  return (
    <div className="st-wide" {...openProps(g.key)}>
      <Img src={heroArt(g.art)} />
      <div className="st-wshade" />
      <div className="st-winfo">
        <div className="st-t">{g.name}</div>
        <Like similar={g.similar} />
        <div className="st-pr"><Price g={g} /></div>
      </div>
    </div>
  )
}

function Picks({ games }: { games: StoreGame[] }) {
  if (games.length < 2) return null
  const [a, b, ...rest] = games
  return (
    <section style={H(531)} className="st-row">
      <div className="st-rh"><h3>Picked for you</h3></div>
      <div className="st-picks">
        <Wide g={a} />
        <Wide g={b} />
        {[rest.slice(0, 2), rest.slice(2, 4)].filter((c) => c.length).map((col, k) => (
          <div className="st-pcol" key={k}>
            {col.map((g) => <ColCard key={g.key} g={g} />)}
          </div>
        ))}
      </div>
    </section>
  )
}

function ColCard({ g }: { g: StoreGame }) {
  return (
    <div className="st-card" {...openProps(g.key)}>
      <div className="st-cv"><Img src={headerArt(g.art)} />{g.wishlisted && <span className="st-wl"><Heart on /></span>}</div>
      <div className="st-t">{g.name}</div>
      <div className="st-pr"><Price g={g} /></div>
      <Like similar={g.similar} />
    </div>
  )
}

function Top({ games }: { games: StoreGame[] }) {
  const top = games.slice(0, 10)
  if (!top.length) return null
  const rank = (g: StoreGame, n: number) => (
    <div className="st-rank" key={g.key} {...openProps(g.key)}>
      <span className="n">{n}</span>
      <div className="st-rimg"><Img src={headerArt(g.art)} /></div>
      <div className="st-rn"><b>{g.name}</b><span>{g.genres.slice(0, 2).join(', ')}</span></div>
      <div className="st-pr"><Price g={g} /></div>
    </div>
  )
  return (
    <section style={H(430)} className="st-row">
      <div className="st-rh"><h3>Top games</h3><span>This week's best-selling games</span></div>
      <div className="st-ranks">
        <div>{top.slice(0, 5).map((g, k) => rank(g, k + 1))}</div>
        <div>{top.slice(5).map((g, k) => rank(g, k + 6))}</div>
      </div>
    </section>
  )
}

function Deal({ g }: { g: StoreGame }) {
  const epic = priceOn(g, 'epic')
  const steam = priceOn(g, 'steam')
  const lead = steam ?? epic
  const adv = advantage(g)
  return (
    <div className="st-deal" {...openProps(g.key)}>
      <Img src={headerArt(g.art)} />
      <span className="st-dbig">-{bestDiscount(g)}%</span>
      <div className="st-dinfo">
        <div className="st-t">{g.name}</div>
        {lead && <div className="st-pr"><span className="st-was">{money(lead.original)}</span><span className="st-now">{money(lead.final)}</span></div>}
        <div className="st-ep">
          {steam && epic
            ? <>Epic {money(epic.final)}{epic.discountPct > 0 && ` (-${epic.discountPct}%)`}</>
            : `${storeName(lead?.store ?? 'steam')} only`}
        </div>
        {adv && <span className={adv.better ? 'st-cheap' : 'st-same'}>{adv.text}</span>}
      </div>
    </div>
  )
}

function Deals({ games }: { games: StoreGame[] }) {
  if (!games.length) return null
  return (
    <section style={H(336)} className="st-row">
      <div className="st-rh"><h3>Deals</h3><span>On sale now</span></div>
      <div className="st-deals">{games.slice(0, 4).map((g) => <Deal key={g.key} g={g} />)}</div>
    </section>
  )
}

function Under({ front }: { front: StoreFront }) {
  const picked = useStoreData((s) => s.under)
  const cur = useStoreData((s) => s.settings?.displayCurrency) ?? front.under.games[0]?.prices[0]?.final.currency ?? 'USD'
  const u = picked ?? front.under
  const base = tiersFor(cur)
  const tiers = base.includes(front.under.tierMinor) ? base : [...base, front.under.tierMinor].sort((a, b) => a - b)
  return (
    <section style={H(371)} className="st-row" id="st-under">
      <div className="st-rh">
        <h3>Under</h3>
        <div className="st-tiers">
          {tiers.map((t) => (
            <button key={t} className={`st-pill${t === u.tierMinor ? ' on' : ''}`} onClick={() => void loadTier(t)}>{tierLabel(t, cur)}</button>
          ))}
        </div>
        <Hidden n={u.hidden} onShow={() => void loadTier(u.tierMinor, true)} />
      </div>
      <div className="st-grid6">
        {u.games.slice(0, 12).map((g) => (
          <div className="st-mini" key={g.key} {...openProps(g.key)}>
            <Img src={headerArt(g.art)} />
            <div className="st-mt"><b>{g.name}</b><span className="st-pr"><Price g={g} /></span></div>
          </div>
        ))}
      </div>
    </section>
  )
}

function NewReleases({ front }: { front: StoreFront }) {
  const shown = useStoreData((s) => s.newReleases)
  const data = shown ?? front.newReleases
  if (!data.games.length) return null
  return (
    <section style={H(421)} className="st-row">
      <div className="st-rh"><h3>Popular new releases</h3><Hidden n={data.hidden} onShow={() => void showHiddenNew()} /></div>
      <div className="st-rl">{data.games.map((g) => <Card key={g.key} g={g} extra={g.release} />)}</div>
    </section>
  )
}

function Wishlist({ games }: { games: StoreGame[] }) {
  const onSale = games.some((g) => bestDiscount(g) > 0)
  const sorted = useMemo(() => [...games].sort((a, b) => bestDiscount(b) - bestDiscount(a)), [games])
  return (
    <section style={H(212)} className="st-row">
      <div className="st-rh"><h3>Your wishlist</h3><button className="st-more" onClick={openWishlist}>Open wishlist</button></div>
      {!onSale && <div className="st-note">Nothing on your wishlist is on sale right now.</div>}
      <div className="st-strip">
        {sorted.map((g) => (
          <div className="st-sm1" key={g.key} {...openProps(g.key)}>
            <div className="st-simg"><Img src={headerArt(g.art)} /></div>
            <span className="st-pr"><Price g={g} /></span>
          </div>
        ))}
      </div>
    </section>
  )
}

function EpicFree({ items }: { items: StoreFront['epicFree'] }) {
  if (!items.length) return null
  return (
    <section style={H(216)} className="st-row">
      <div className="st-rh"><h3>Free on Epic</h3></div>
      <div className="st-free">
        {items.map(({ game: g, now }) => {
          const p = g.prices[0]
          return (
            <div className="st-fcard" key={g.key} {...openProps(g.key)}>
              <div className="st-fimg"><Img src={coverArt(g.art)} /></div>
              <div className="st-fi">
                <span className={`st-fb${now ? ' now' : ''}`}>{now ? 'Free now' : 'Free next week'}</span>
                <b>{g.name}</b>
                {p && <span className="st-was">{money(p.original)}</span>}
              </div>
            </div>
          )
        })}
      </div>
    </section>
  )
}

function Genres({ items, pick }: { items: StoreFront['genres']; pick: (g: string) => void }) {
  if (!items.length) return null
  return (
    <section style={H(176)} className="st-row">
      <div className="st-rh"><h3>Browse</h3></div>
      <div className="st-genres">
        {items.map((g) => (
          <button className="st-gt" key={g.name} onClick={() => pick(g.name)}><Img src={g.art} /><span>{g.name}</span></button>
        ))}
      </div>
    </section>
  )
}

/** Search + filters replace the rows until cleared. */
function Results({ q, filters }: { q: string; filters: StoreFilters }) {
  const [games, setGames] = useState<StoreGame[] | null>(null)
  const settings = useStoreData((s) => s.settings)
  const seq = useRef(0)
  const key = JSON.stringify([q, filters, settings?.stores, settings?.hideAdult, settings?.displayCurrency])
  useEffect(() => {
    const mine = ++seq.current
    const t = setTimeout(() => {
      storeApi.search(q.trim(), filters)
        .then((r) => { if (mine === seq.current) setGames(r) })
        .catch(() => { if (mine === seq.current) setGames([]) })
    }, 220)
    return () => clearTimeout(t)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key])
  return (
    <section className="st-results">
      <div className="st-rh"><h3>{q.trim() ? 'Results' : (filters.genre ?? 'Games')}</h3></div>
      {games && !games.length && <div className="st-note">Nothing found.</div>}
      <div className="st-rgrid">{(games ?? []).map((g) => <Card key={g.key} g={g} />)}</div>
    </section>
  )
}

export function Front() {
  const front = useStoreData((s) => s.front)
  const failed = useStoreData((s) => s.failed)
  const q = useStoreSearch((s) => s.q)
  const filters = useStoreSearch((s) => s.filters)
  const setQ = (q: string) => storeSearch.set({ q })
  const setFilters = (filters: StoreFilters) => storeSearch.set({ filters })
  useEffect(() => startStore(), [])
  const searching = q.trim() !== '' || filters.genre != null
  const top = useRef<HTMLDivElement>(null)
  // Starting a search brings the store bar to the top so the results are in view.
  useEffect(() => {
    if (!searching) return
    const sc = top.current?.closest('.app')
    const hero = top.current?.querySelector('.st-hero') as HTMLElement | null
    if (sc && hero) sc.scrollTo({ top: Math.max(sc.scrollTop, hero.offsetHeight - 164), behavior: 'smooth' })
  }, [searching])
  if (!front) {
    return (
      <div className="st-empty">
        {failed ? <><span>Can't reach the store</span><button className="st-btn" onClick={() => { storeData.set({ failed: false }); void storeApi.front().then((f) => storeData.set({ front: f })).catch(() => storeData.set({ failed: true })) }}>Retry</button></> : null}
      </div>
    )
  }
  return (
    <div ref={top}>
      <Hero games={front.featured} />
      <Bar front={front} q={q} setQ={setQ} filters={filters} setFilters={setFilters} />
      {searching ? <Results q={q} filters={filters} /> : (
        <>
          <Picks games={front.picked} />
          <Top games={front.top} />
          <Deals games={front.deals} />
          <Under front={front} />
          <NewReleases front={front} />
          <Wishlist games={front.wishlist} />
          <EpicFree items={front.epicFree} />
          <Genres items={front.genres} pick={(genre) => setFilters({ ...filters, genre })} />
          <div style={{ height: 40 }} />
        </>
      )}
    </div>
  )
}
