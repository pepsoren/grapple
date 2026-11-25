// Settings › Playing: closing Steam after a game, and on Linux the Proton, GameMode and MangoHud defaults for the
// games grapple starts itself (Epic, added by hand), the game runtime, the Proton versions found.
import { useEffect, useState } from 'react'
import { api } from '../../lib/ipc'
import { useLinux } from '../../lib/platform'
import { updateSettings, useSettings } from '../../lib/settings'
import type { ProtonsInfo } from '../../lib/types'
import { Btn, Choice, Group, Match, Pane, Row, Switch } from './kit'

const SOURCE: Record<ProtonsInfo['versions'][number]['source'], string> = {
  heroic: "From Heroic's tools",
  'steam-tools': "From Steam's tools folder",
  steam: 'From Steam',
}

/** The protons list, asked once per mount (Linux only). */
export function useProtons(on: boolean): ProtonsInfo | null {
  const [p, setP] = useState<ProtonsInfo | null>(null)
  useEffect(() => {
    if (on) api.protons().then(setP, () => setP(null))
  }, [on])
  return p
}

/** "Runs with" choices: Heroic's own, then every version found. */
export function protonOptions(p: ProtonsInfo | null, heroicLabel = 'As set in Heroic'): [string | null, string][] {
  return [[null, heroicLabel], ...(p?.choices ?? []).map((c): [string, string] => [c, c])]
}

export function PlayingPane() {
  const s = useSettings((x) => x)
  const linux = useLinux()
  const protons = useProtons(linux)
  const isDefault = (name: string) => (s.default_proton ? s.default_proton === name : protons?.heroic_default === name)
  return (
    <Pane title="Playing">
      <Group title="Steam games">
        <Row label="Close Steam when I quit a game" sub="Unless Steam was already open">
          <Switch on={s.close_steam} onChange={(v) => void updateSettings({ close_steam: v })} />
        </Row>
      </Group>
      {linux && (
        <>
          <Group title="Epic and other Windows games" os="Linux">
            <Row label="Runs with" keywords="proton wine compatibility">
              <Choice value={s.default_proton} options={protonOptions(protons)} onChange={(v) => void updateSettings({ default_proton: v })} />
            </Row>
            <Row label="Update game runtime" sub="While no game runs" keywords="umu steam linux runtime">
              <Switch on={s.update_runtime} onChange={(v) => void updateSettings({ update_runtime: v })} />
            </Row>
          </Group>
          <Group title="Defaults for every game" os="Linux">
            <Row label="GameMode" sub="Asks for full speed">
              <Switch on={s.default_gamemode} onChange={(v) => void updateSettings({ default_gamemode: v })} />
            </Row>
            <Row label="MangoHud" sub="FPS and temperatures">
              <Switch on={s.default_mangohud} onChange={(v) => void updateSettings({ default_mangohud: v })} />
            </Row>
          </Group>
          {protons && protons.versions.length > 0 && (
            <Group title="Proton versions" os="Linux">
              {protons.versions.map((v) => (
                <Match key={v.name} label={`${v.name} proton version default`}>
                  <div className="set-proton">
                    <div>
                      <b>{v.name}</b>{isDefault(v.name) && <span className="set-tag">Default</span>}
                      <div className="sub">{SOURCE[v.source]}</div>
                    </div>
                    <span className="sub">Installed</span>
                    {isDefault(v.name) ? <span /> : <Btn onClick={() => void updateSettings({ default_proton: v.name })}>Make default</Btn>}
                  </div>
                </Match>
              ))}
            </Group>
          )}
        </>
      )}
    </Pane>
  )
}
