// A game's settings (mockups/08-settings.html, screen C), from its right-click menu or the Details drawer: General,
// Launch, Controller, Linux and Files tabs. Choices here override the defaults in Settings for this one game.
import { useEffect, useState } from 'react'
import { storeLabel } from '../../lib/format'
import { toggleFavourite, useGames } from '../../lib/games'
import { api } from '../../lib/ipc'
import { removeManual } from '../../lib/library'
import { useLinux } from '../../lib/platform'
import { updateSettings, useSettings } from '../../lib/settings'
import type { Game, GameFiles, GameSettings } from '../../lib/types'
import { closeGameSheet, toast, uiStore, useUi, type GameSheetTab } from '../../lib/ui'
import { backdrop } from '../Art'
import { Confirm } from '../downloads/parts'
import { CloseIcon } from '../Icons'
import { GameControllerTab } from './Controllers'
import { Btn, Choice, Group, Row, Switch, bytes } from './kit'
import { protonOptions, useProtons } from './Playing'
import '../../styles/downloads.css'
import '../../styles/store-page.css'
import '../../styles/settings.css'

const onOff = (v: boolean) => (v ? 'On' : 'Off')
/** "Default · On" / On / Off for a choice that can follow the default. */
const triple = (def: boolean): [boolean | null, string][] => [[null, `Default · ${onOff(def)}`], [true, 'On'], [false, 'Off']]

function GeneralTab({ g, gs, set }: { g: Game; gs: GameSettings; set: (p: Partial<GameSettings>) => void }) {
  const hidden = useSettings((s) => s.hidden_games)
  const isHidden = hidden.includes(g.id)
  return (
    <Group title="This game">
      <Row label="Show it to friends when I play">
        <Switch on={gs.show_in_tether} onChange={(v) => set({ show_in_tether: v })} />
      </Row>
      <Row label="Favourite">
        <Switch on={g.favourite} onChange={() => void toggleFavourite(g.id)} />
      </Row>
      <Row label="Hide from my library">
        <Switch on={isHidden} onChange={(v) => void updateSettings({ hidden_games: v ? [...hidden, g.id] : hidden.filter((h) => h !== g.id) })} />
      </Row>
    </Group>
  )
}

function LaunchTab({ g, gs, set }: { g: Game; gs: GameSettings; set: (p: Partial<GameSettings>) => void }) {
  const closeDefault = useSettings((s) => s.close_steam)
  const [overlayBusy, setOverlayBusy] = useState(false)
  // The first switch-on downloads Epic's overlay, so it can take a moment.
  const setOverlay = (on: boolean) => {
    setOverlayBusy(true)
    if (on) toast("Getting Epic's overlay…")
    api.eosOverlaySet(g.id, on).then(
      () => { set({ eos_overlay: on }); if (on) toast('Epic overlay on') },
      (e) => toast(String(e)),
    ).finally(() => setOverlayBusy(false))
  }
  return (
    <Group title="Launch">
      {g.store === 'steam' && (
        <Row label="Close Steam when I quit">
          {/* The default leaves a Steam that was already open; this game's own choice is always / never. */}
          <Choice
            value={gs.close_steam_on_quit ?? null}
            options={[[null, `Default · ${onOff(closeDefault)}`], [true, 'Always'], [false, 'Never']]}
            onChange={(v) => set({ close_steam_on_quit: v })}
          />
        </Row>
      )}
      {g.store === 'epic' && (
        <Row label="Epic overlay" sub="Store purchases in game">
          <Switch on={!!gs.eos_overlay} disabled={overlayBusy} onChange={setOverlay} />
        </Row>
      )}
    </Group>
  )
}

function LinuxTab({ g, gs, set }: { g: Game; gs: GameSettings; set: (p: Partial<GameSettings>) => void }) {
  const s = useSettings((x) => x)
  const protons = useProtons(g.store === 'epic')
  const def = s.default_proton ?? 'As set in Heroic'
  return (
    <>
      {g.store === 'epic' && (
        <Group title="Compatibility">
          <Row label="Runs with" keywords="proton">
            <Choice value={gs.proton ?? null} options={protonOptions(protons, `Default · ${def}`)} onChange={(v) => set({ proton: v })} />
          </Row>
        </Group>
      )}
      <Group title="Extras">
        <Row label="GameMode">
          <Choice value={gs.gamemode ?? null} options={triple(s.default_gamemode)} onChange={(v) => set({ gamemode: v })} />
        </Row>
        <Row label="MangoHud">
          <Choice value={gs.mangohud ?? null} options={triple(s.default_mangohud)} onChange={(v) => set({ mangohud: v })} />
        </Row>
      </Group>
    </>
  )
}

function FilesTab({ g }: { g: Game }) {
  const [f, setF] = useState<GameFiles | null>(null)
  const [uninstall, setUninstall] = useState(false)
  useEffect(() => { api.gameFiles(g.id).then(setF, () => setF({ folder: null, size_bytes: null })) }, [g.id, g.installed])
  const store = g.store !== 'manual'
  return (
    <>
      {f?.folder && (
        <Group title="Installed">
          <Row label="Folder" sub={f.folder} mono>
            <Btn onClick={() => void api.openPlace('game', g.id).catch((e) => toast(String(e)))}>Open</Btn>
          </Row>
          {f.size_bytes != null && <Row label="Size"><span className="set-val">{bytes(f.size_bytes)}</span></Row>}
        </Group>
      )}
      {f && !f.folder && <div className="set-empty">{g.installed ? 'Folder not found' : 'Not installed'}</div>}
      <div className="set-files">
        {g.installed && store && <Btn danger onClick={() => setUninstall(true)}>Uninstall</Btn>}
        {g.store === 'manual' && <Btn danger onClick={() => { closeGameSheet(); void removeManual(g) }}>Remove from grapple</Btn>}
      </div>
      {uninstall && (
        <Confirm
          title={`Uninstall ${g.name}?`}
          yes="Uninstall"
          onNo={() => setUninstall(false)}
          onYes={() => {
            setUninstall(false)
            api.uninstallGame(g.id).then(() => toast(`Uninstalling ${g.name}`), (e) => toast(String(e)))
          }}
        />
      )}
    </>
  )
}

function Sheet({ g, tab }: { g: Game; tab: GameSheetTab }) {
  const linux = useLinux()
  const [gs, setGs] = useState<GameSettings | null>(null)
  useEffect(() => { api.getGameSettings(g.id).then(setGs, () => setGs(null)) }, [g.id])
  useEffect(() => {
    const key = (e: KeyboardEvent) => { if (e.key === 'Escape') { e.stopPropagation(); closeGameSheet() } }
    document.addEventListener('keydown', key)
    return () => document.removeEventListener('keydown', key)
  }, [])
  // Read the stored settings again before saving: the Controller tab saves on its own, and an older copy here
  // would write its choice back.
  const set = async (patch: Partial<GameSettings>) => {
    if (!gs) return
    const cur = await api.getGameSettings(g.id).catch(() => gs)
    const next = { ...cur, ...patch }
    setGs(next)
    api.setGameSettings(g.id, next).catch((e) => toast(String(e)))
  }
  // Steam starts its own games: grapple controls neither their Proton nor GameMode / MangoHud.
  const tabs: [GameSheetTab, string][] = [
    ['general', 'General'],
    ...(g.store !== 'manual' ? [['launch', 'Launch'] as [GameSheetTab, string]] : []),
    ['controller', 'Controller'],
    ...(linux && g.store !== 'steam' ? [['linux', 'Linux'] as [GameSheetTab, string]] : []),
    ['files', 'Files'],
  ]
  const cur = tabs.some(([k]) => k === tab) ? tab : 'general'
  const bd = backdrop(g)
  return (
    <div className="gs-scrim" onClick={closeGameSheet}>
      <div className="gs" role="dialog" aria-modal aria-label={`${g.name} settings`} onClick={(e) => e.stopPropagation()}>
        <div className="gs-art">
          {bd.url && <img src={bd.url} alt="" className={bd.blur ? 'blur' : undefined} />}
          <div className="gs-ttl"><h3>{g.name}</h3><span className="gs-badge">{storeLabel(g)}</span></div>
          <button className="gs-x" title="Close (Esc)" onClick={closeGameSheet}><CloseIcon /></button>
        </div>
        <div className="gs-tabs" role="tablist">
          {tabs.map(([k, label]) => (
            <button key={k} role="tab" aria-selected={k === cur} className={k === cur ? 'on' : ''} onClick={() => uiStore.set({ gameSheet: { id: g.id, tab: k } })}>{label}</button>
          ))}
        </div>
        <div className="gs-body">
          {gs && cur === 'general' && <GeneralTab g={g} gs={gs} set={set} />}
          {gs && cur === 'launch' && <LaunchTab g={g} gs={gs} set={set} />}
          {cur === 'controller' && <GameControllerTab g={g} />}
          {gs && cur === 'linux' && <LinuxTab g={g} gs={gs} set={set} />}
          {cur === 'files' && <FilesTab g={g} />}
        </div>
      </div>
    </div>
  )
}

export default function GameSheet() {
  const sheet = useUi((s) => s.gameSheet)
  const g = useGames((s) => (sheet ? s.all.find((x) => x.id === sheet.id) : undefined))
  if (!sheet || !g) return null
  return <Sheet g={g} tab={sheet.tab} />
}
