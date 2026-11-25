// Settings › Advanced: grapple's cache and data folders, the game logs, and resetting the settings.
import { useEffect, useState } from 'react'
import { api } from '../../lib/ipc'
import { resetSettings } from '../../lib/settings'
import type { StorageInfo } from '../../lib/types'
import { toast } from '../../lib/ui'
import { Confirm } from '../downloads/parts'
import { Btn, Group, Pane, Row, bytes, copyText } from './kit'

export function AdvancedPane() {
  const [st, setSt] = useState<StorageInfo | null>(null)
  const [busy, setBusy] = useState(false)
  const [asking, setAsking] = useState(false)
  const load = () => api.storageInfo().then(setSt, () => setSt(null))
  useEffect(() => { void load() }, [])
  const open = (place: 'data' | 'logs') => api.openPlace(place).catch((e) => toast(String(e)))
  const clear = () => {
    setBusy(true)
    api.cacheClear().then((n) => toast(`Cleared ${bytes(n)}`), (e) => toast(String(e))).finally(() => { setBusy(false); void load() })
  }
  const copyLog = async () => {
    try {
      const log = await api.lastLaunchLog()
      toast((await copyText(log)) ? 'Last launch copied' : 'Could not copy')
    } catch (e) {
      toast(String(e))
    }
  }
  return (
    <Pane title="Advanced">
      {st && (
        <Group title="Storage">
          <Row label="Pictures and store cache" sub={st.cache_path} mono keywords="clear cache">
            <span className="sub">{bytes(st.cache_bytes)}</span>
            <Btn disabled={busy || st.cache_bytes === 0} onClick={clear}>Clear</Btn>
          </Row>
          <Row label="grapple's data" sub={st.data_path} mono keywords="folder database">
            <span className="sub">{bytes(st.data_bytes)}</span>
            <Btn onClick={() => void open('data')}>Open folder</Btn>
          </Row>
        </Group>
      )}
      {st && (
        <Group title="When something breaks">
          <Row label="Logs" sub={st.logs_path} mono keywords="log debug launch">
            <Btn onClick={() => void open('logs')}>Open folder</Btn>
            {st.has_log && <Btn onClick={() => void copyLog()}>Copy last launch</Btn>}
          </Row>
        </Group>
      )}
      <Group title="Start over" className="set-dz">
        <Row label="Reset settings" sub="Keeps playtime and sign-ins">
          <Btn danger onClick={() => setAsking(true)}>Reset</Btn>
        </Row>
      </Group>
      {asking && (
        <Confirm
          title="Reset settings?"
          text="grapple's settings go back to how they started. Playtime, favourites and sign-ins stay."
          yes="Reset"
          onNo={() => setAsking(false)}
          onYes={() => {
            setAsking(false)
            resetSettings().then(() => toast('Settings reset'), (e) => toast(String(e)))
          }}
        />
      )}
    </Pane>
  )
}
