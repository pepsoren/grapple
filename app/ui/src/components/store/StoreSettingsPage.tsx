// Settings › Store (mockups/08-settings.html; was the store's own settings page, mockups/06-store.html D): display
// currency, the Steam region, the stores, the IsThereAnyDeal key, the adult/violence filters, sale alerts and the
// wishlist games removed here.
import { useEffect, useMemo, useState } from 'react'
import { setItadKey, setStoreRegion, showHiddenWishlist, startStore, updateStoreSettings, useStoreData } from '../../lib/store-data'
import { countryList, countryName } from '../../lib/store-countries'
import { currencySymbol } from '../../lib/store-format'
import type { StoreKind } from '../../lib/store-types'
import { Btn, Group, Pane, Row, Switch } from '../settings/kit'
import { Pop } from './Pop'

/** The country menu: current first, a filter on top. */
function Countries({ current, manual, close }: { current: string; manual: boolean; close: () => void }) {
  const [q, setQ] = useState('')
  const list = useMemo(() => countryList(current), [current])
  const shown = q.trim() ? list.filter((c) => c.name.toLowerCase().includes(q.trim().toLowerCase())) : list
  const pick = (code: string | null) => { void setStoreRegion(code); close() }
  return (
    <>
      <input className="st-menusearch" autoFocus value={q} placeholder="Search" spellCheck={false} onChange={(e) => setQ(e.target.value)} onKeyDown={(e) => { if (e.key === 'Enter' && shown[0]) pick(shown[0].code) }} />
      {manual && !q && <button onClick={() => pick(null)}>Automatic</button>}
      {shown.map((c) => (
        <button key={c.code} className={c.code === current ? 'on' : ''} onClick={() => pick(c.code)}>
          {c.name}{c.code === current && <i>✓</i>}
        </button>
      ))}
    </>
  )
}

function Region() {
  const region = useStoreData((x) => x.region)
  if (!region) return null
  return (
    <Row label="Steam region" keywords="country">
      <Pop label={<>{countryName(region.country)} ▾</>} className="set-pill" right>
        {(close) => <Countries current={region.country} manual={region.source === 'setting'} close={close} />}
      </Pop>
    </Row>
  )
}

function ItadKey({ set }: { set: boolean }) {
  const [value, setValue] = useState('')
  const [busy, setBusy] = useState(false)
  const save = async () => {
    const key = value.trim()
    if (!key || busy) return
    setBusy(true)
    if (await setItadKey(key)) setValue('')
    setBusy(false)
  }
  return (
    <Row label="IsThereAnyDeal key" sub="Epic prices" keywords="itad api">
      <span className="sub">{set ? 'Set' : 'Not set'}</span>
      <input
        className="set-input"
        type="password"
        value={value}
        placeholder="Paste key"
        autoComplete="off"
        spellCheck={false}
        aria-label="IsThereAnyDeal key"
        onChange={(e) => setValue(e.target.value)}
        onKeyDown={(e) => { if (e.key === 'Enter') void save(); e.stopPropagation() }}
      />
      <Btn disabled={!value.trim() || busy} onClick={() => void save()}>Save</Btn>
      {set && <Btn disabled={busy} onClick={() => { setBusy(true); void setItadKey(null).finally(() => setBusy(false)) }}>Remove</Btn>}
    </Row>
  )
}

export function StoreSettingsPane() {
  const s = useStoreData((x) => x.settings)
  const currencies = useStoreData((x) => x.currencies)
  const hidden = useStoreData((x) => x.hiddenWishlist.length)
  useEffect(() => startStore(), [])
  if (!s) return <Pane title="Store">{null}</Pane>
  const cur = currencies.find((c) => c.code === s.displayCurrency)
  const sym = currencySymbol(s.displayCurrency)
  // One store stays on.
  const toggle = (k: StoreKind) => {
    if (s.stores[k] && !s.stores[k === 'steam' ? 'epic' : 'steam']) return
    void updateStoreSettings({ stores: { ...s.stores, [k]: !s.stores[k] } })
  }
  return (
    <Pane title="Store">
      <Group title="Prices">
        <Row label="Show prices in" keywords="currency">
          <Pop label={<>{sym !== s.displayCurrency ? `${sym} · ` : ''}{cur?.name ?? s.displayCurrency} ▾</>} className="set-pill" right>
            {(close) => currencies.map((c) => (
              <button key={c.code} className={c.code === s.displayCurrency ? 'on' : ''} onClick={() => { void updateStoreSettings({ displayCurrency: c.code }); close() }}>
                {c.name}{c.code === s.displayCurrency && <i>✓</i>}
              </button>
            ))}
          </Pop>
        </Row>
        <Region />
        <Row label="Stores" keywords="steam epic">
          <span className="sub">Steam</span>
          <Switch on={s.stores.steam} label="Steam" onChange={() => toggle('steam')} />
          <span className="sub set-gap-l">Epic</span>
          <Switch on={s.stores.epic} label="Epic" onChange={() => toggle('epic')} />
        </Row>
        <ItadKey set={s.itadKey === true} />
      </Group>
      <Group title="Filters">
        <Row label="Hide adult games"><Switch on={s.hideAdult} onChange={(v) => void updateStoreSettings({ hideAdult: v })} /></Row>
        <Row label="Hide very violent games"><Switch on={s.hideViolent} onChange={(v) => void updateStoreSettings({ hideViolent: v })} /></Row>
      </Group>
      <Group title="Wishlist">
        <Row label="Tell me when something goes on sale" keywords="wishlist alerts notifications">
          <Switch on={s.wishlistAlerts} onChange={(v) => void updateStoreSettings({ wishlistAlerts: v })} />
        </Row>
        {hidden > 0 && (
          <Row label="Removed from my wishlist here" sub="Still on your Steam wishlist">
            <span className="sub">{hidden}</span>
            <Btn onClick={() => void showHiddenWishlist()}>Show</Btn>
          </Row>
        )}
      </Group>
    </Pane>
  )
}
