// Settings › Downloads (mockups/08-settings.html; the settings of mockups/07-downloads.html D): speed, updates, where
// new games go, Steam download control. "Tell me when a game is ready" is in Settings › Notifications.
import { useEffect, useState } from 'react'
import { loadDownloadSettings, setSteamBridge, updateDownloadSettings, useDownloads } from '../../lib/downloads'
import { Choice, Group, Pane, Row, Switch } from '../settings/kit'
import { Confirm } from './parts'

const LIMITS = [0, 5, 10, 25, 50, 100].map((mb) => mb * 1_000_000)
const limitText = (b: number) => (b ? `${b / 1_000_000} MB/s` : 'No limit')

/** A folder pill: the current folder first when it is not one of the choices. */
function Folder({ value, choices, onPick }: { value: string; choices: string[]; onPick: (v: string) => void }) {
  const list = choices.includes(value) || !value ? choices : [value, ...choices]
  return <Choice mono value={value} label={value || 'Steam decides'} options={list.map((c): [string, string] => [c, c])} onChange={onPick} />
}

export function DownloadSettingsPane() {
  const s = useDownloads((x) => x.settings)
  const bridge = useDownloads((x) => x.snap?.steamBridge)
  const [asking, setAsking] = useState(false)
  useEffect(() => { void loadDownloadSettings() }, [])
  if (!s) return <Pane title="Downloads">{null}</Pane>
  const set = updateDownloadSettings
  return (
    <Pane title="Downloads">
      <Group title="Speed">
        <Row label="Speed limit" keywords="bandwidth">
          <Choice value={s.speedLimitBytes} options={LIMITS.map((l): [number, string] => [l, limitText(l)])} onChange={(v) => void set({ speedLimitBytes: v })} />
        </Row>
        <Row label="Pause downloads while I'm playing">
          <Switch on={s.pauseWhilePlaying} onChange={(v) => void set({ pauseWhilePlaying: v })} />
        </Row>
      </Group>
      <Group title="Updates">
        <Row label="Update games automatically">
          <Switch on={s.autoUpdate} onChange={(v) => void set({ autoUpdate: v })} />
        </Row>
        {s.autoUpdate && (
          <Row label="Only while I'm not playing" sub1>
            <Switch on={s.updateOnlyWhenIdle} onChange={(v) => void set({ updateOnlyWhenIdle: v })} />
          </Row>
        )}
      </Group>
      <Group title="Where new games go">
        <Row label="Steam games" keywords="folder library install">
          <Folder value={s.steamDefaultDir ?? s.choices?.steam[0] ?? ''} choices={s.choices?.steam ?? []} onPick={(v) => void set({ steamDefaultDir: v })} />
        </Row>
        <Row label="Epic games" keywords="folder install">
          <Folder value={s.epicDefaultDir} choices={s.choices?.epic ?? []} onPick={(v) => void set({ epicDefaultDir: v })} />
        </Row>
      </Group>
      <Group title="Steam">
        <Row
          label="Steam download control"
          sub={bridge === 'restart-pending' ? 'Restarts Steam after your game' : s.steamBridge && bridge === 'unavailable' ? "Steam isn't responding" : undefined}
          keywords="bridge pause reorder"
        >
          <Switch on={s.steamBridge} onChange={(v) => (v ? setAsking(true) : void setSteamBridge(false))} />
        </Row>
      </Group>
      {asking && (
        <Confirm
          title="Steam download control"
          text="Lets grapple pause, reorder and install Steam games. Any program on this PC can control Steam while it's on."
          yes="Turn on"
          onYes={() => { setAsking(false); void setSteamBridge(true) }}
          onNo={() => setAsking(false)}
        />
      )}
    </Pane>
  )
}
