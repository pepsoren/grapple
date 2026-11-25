// Settings › Library: where games come from (per-store on/off), the folders grapple reads, what shows up, hidden
// games, the playtime week, rescan.
import { useEffect, useState } from 'react'
import { refreshGames } from '../../lib/games'
import { api, events } from '../../lib/ipc'
import { updateSettings, useSettings } from '../../lib/settings'
import type { LibraryInfo, Store } from '../../lib/types'
import { openAddGame, toast } from '../../lib/ui'
import { EpicMark, FolderIcon, SteamMark } from './icons'
import { Btn, Dialog, Group, Match, Pane, Row, Seg, Switch, agoShort, bytes } from './kit'

const STORE_NAME: Record<Store, string> = { steam: 'Steam', epic: 'Epic Games', manual: 'Other games' }
/** Under this much free space a folder's number turns amber. */
const LOW_SPACE = 10e9

function useLibraryInfo(): [LibraryInfo | null, () => void] {
  const [info, setInfo] = useState<LibraryInfo | null>(null)
  const load = () => void api.libraryInfo().then(setInfo, () => setInfo(null))
  useEffect(() => {
    load()
    let off: (() => void) | undefined
    void events.gamesChanged(load).then((u) => { off = u })
    return () => off?.()
  }, [])
  return [info, load]
}

function HiddenGames({ info }: { info: LibraryInfo }) {
  const hidden = useSettings((s) => s.hidden_games)
  const [open, setOpen] = useState(false)
  const unhide = (id: string) => void updateSettings({ hidden_games: hidden.filter((h) => h !== id) })
  return (
    <>
      <Row label="Hidden games" keywords="unhide">
        <span className="sub">{info.hidden.length || 'None'}</span>
        {info.hidden.length > 0 && <Btn onClick={() => setOpen(true)}>Show</Btn>}
      </Row>
      {open && info.hidden.length > 0 && (
        <Dialog title="Hidden games" onClose={() => setOpen(false)}>
          {info.hidden.map((g) => (
            <div key={g.id} className="set-li"><span>{g.name}</span><Btn onClick={() => unhide(g.id)}>Unhide</Btn></div>
          ))}
        </Dialog>
      )}
    </>
  )
}

export function LibraryPane() {
  const s = useSettings((x) => x)
  const [info, reload] = useLibraryInfo()
  const [scanning, setScanning] = useState(false)
  const storeOn = (k: string) => !s.hidden_stores.includes(k)
  const setStore = (k: string, on: boolean) => void updateSettings({ hidden_stores: on ? s.hidden_stores.filter((x) => x !== k) : [...s.hidden_stores, k] })
  const rescan = () => {
    setScanning(true)
    api.rescan().then(() => refreshGames(), (e) => toast(String(e))).finally(() => { setScanning(false); reload() })
  }
  return (
    <Pane title="Library">
      {info && (
        <Group title="Where your games come from">
          {info.sources.map((src) => (
            <Match key={src.store} label={`${STORE_NAME[src.store]} source store add a game`}>
              <div className="set-src">
                <span className="ic">{src.store === 'steam' ? <SteamMark /> : src.store === 'epic' ? <EpicMark /> : <FolderIcon />}</span>
                <div>
                  <b>{STORE_NAME[src.store]}</b>
                  <div className="k">{src.store === 'manual' ? `Added by hand · ${src.games}` : `${src.games} games · ${src.installed} installed`}</div>
                </div>
                {src.store === 'manual'
                  ? <Btn onClick={openAddGame}>Add a game</Btn>
                  : <Switch on={storeOn(src.store)} label={`Show ${STORE_NAME[src.store]} games`} onChange={(v) => setStore(src.store, v)} />}
              </div>
            </Match>
          ))}
        </Group>
      )}
      {info && info.folders.length > 0 && (
        <Group title="Folders grapple watches">
          {info.folders.map((f) => (
            <Match key={f.path} label={`${f.path} folder library drive free space`}>
              <div className="set-src">
                <span className="ic muted"><FolderIcon /></span>
                <div>
                  <b className="set-mono strong">{f.path}</b>
                  <div className="k">{f.store === 'steam' ? 'Steam' : 'Epic'} · {f.installed} installed</div>
                </div>
                {f.free_bytes != null && <span className={`sub${f.free_bytes < LOW_SPACE ? ' warn' : ''}`}>{bytes(f.free_bytes)} free</span>}
              </div>
            </Match>
          ))}
        </Group>
      )}
      <Group title="What shows up">
        <Row label="Games I don't have installed" keywords="uninstalled">
          <Switch on={s.show_uninstalled} onChange={(v) => void updateSettings({ show_uninstalled: v })} />
        </Row>
        {info && <HiddenGames info={info} />}
      </Group>
      <Group title="Playtime">
        <Row label="Week starts on" keywords="this week monday sunday">
          <Seg value={s.week_start} options={[['monday', 'Monday'], ['sunday', 'Sunday']]} onChange={(v) => void updateSettings({ week_start: v })} />
        </Row>
      </Group>
      <Group className="set-gap">
        <Row label="Rescan libraries" sub={scanning ? 'Scanning…' : `Last: ${agoShort(info?.last_scan)}`} keywords="refresh">
          <Btn disabled={scanning} onClick={rescan}>Rescan</Btn>
        </Row>
      </Group>
    </Pane>
  )
}
