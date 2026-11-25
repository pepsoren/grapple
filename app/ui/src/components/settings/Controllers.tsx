// Controllers (mockups/08-settings.html: Settings › Controllers, and the Controller tab of a game's settings).
// The pads, their live state and the rumble come from src-tauri/src/controllers; the "In games" choices become
// Proton environment variables when grapple starts a game (crates/grapple-launch/src/pads.rs).
//
// Left out on purpose (no row that does nothing): "Guide button opens grapple" (the Guide/PS button cannot be read
// while grapple is hidden without taking it from games, Steam and the Xbox Game Bar), and the in-game "Swap A/B" and
// "Rumble" switches (neither Proton nor SDL has a switch that does that for every pad).
import { useEffect, useState } from 'react'
import { api, events } from '../../lib/ipc'
import { glyph, promptStyle, refreshPads, startPads, usePads } from '../../lib/pads'
import { useLinux } from '../../lib/platform'
import { updateSettings, useSettings } from '../../lib/settings'
import type { Game, GameSettings, PadButton, PadInGames, PadInfo, PadLayout, PadStateDto } from '../../lib/types'
import { toast } from '../../lib/ui'
import { Btn, Choice, Group, Match, Pane, Row, Seg, Switch } from './kit'
import { PadIcon } from './icons'
import '../../styles/controllers.css'

const CONNECTION = { usb: 'USB', bluetooth: 'Bluetooth', wired: 'Wired' } as const
const IN_GAMES: [PadInGames, string][] = [['as_is', 'Controller as is'], ['xbox', 'Xbox-compatible']]

/** "USB", "72%", "Charging 40%": what the system says about the pad. */
export function padParts(p: Pick<PadInfo, 'connection' | 'battery' | 'charging'>): { conn: string | null; battery: string | null } {
  const conn = p.connection ? CONNECTION[p.connection] : null
  const battery = p.battery == null ? null : `${p.charging && p.battery < 100 ? 'Charging ' : ''}${p.battery}%`
  return { conn, battery }
}

function DevRow({ p, testing, onTest }: { p: PadInfo; testing: boolean; onTest: () => void }) {
  const { conn, battery } = padParts(p)
  return (
    <Match label={`${p.name} controller`}>
      <div className="set-row pad-dev">
        <div className="l">
          <PadIcon />
          <div className="nm">
            <b>{p.name}</b>
            <span className="meta">
              {conn && <>{conn} · </>}
              {battery && (
                <>
                  {battery}
                  <i className={`pad-batt${(p.battery ?? 100) <= 20 && !p.charging ? ' low' : ''}`}><i style={{ width: `${p.battery}%` }} /></i>
                  {' · '}
                </>
              )}
              Player {p.player}
            </span>
          </div>
        </div>
        <div className="r"><Btn onClick={onTest}>{testing ? 'Stop' : 'Test'}</Btn></div>
      </div>
    </Match>
  )
}

const TRIGGERS: Record<PadLayout, [string, string]> = { playstation: ['L2', 'R2'], xbox: ['LT', 'RT'], nintendo: ['ZL', 'ZR'] }
const CHIPS: Record<PadLayout, [PadButton, string][]> = {
  playstation: [['lb', 'L1'], ['rb', 'R1'], ['select', 'Create'], ['start', 'Options'], ['ls', 'L3'], ['rs', 'R3'], ['guide', 'PS']],
  xbox: [['lb', 'LB'], ['rb', 'RB'], ['select', 'View'], ['start', 'Menu'], ['ls', 'LS'], ['rs', 'RS'], ['guide', 'Guide']],
  nintendo: [['lb', 'L'], ['rb', 'R'], ['select', '−'], ['start', '+'], ['ls', 'LS'], ['rs', 'RS'], ['guide', 'Home']],
}

function Stick({ x, y, press }: { x: number; y: number; press: boolean }) {
  const moved = Math.hypot(x, y) > 0.15
  return (
    <svg width="96" height="96" viewBox="0 0 96 96" aria-hidden>
      <circle className="ring" cx="48" cy="48" r="44" />
      <circle className="mid" cx="48" cy="48" r="22" />
      <circle className={`dot${moved || press ? ' on' : ''}`} cx={48 + x * 30} cy={48 + y * 30} r="10" />
    </svg>
  )
}

function Dpad({ on }: { on: Set<PadButton> }) {
  const k = (b: PadButton) => `key${on.has(b) ? ' on' : ''}`
  return (
    <svg width="96" height="96" viewBox="0 0 96 96" aria-hidden>
      <rect className={k('up')} x="36" y="10" width="24" height="28" rx="4" />
      <rect className={k('down')} x="36" y="58" width="24" height="28" rx="4" />
      <rect className={k('left')} x="10" y="36" width="28" height="24" rx="4" />
      <rect className={k('right')} x="58" y="36" width="28" height="24" rx="4" />
    </svg>
  )
}

const FACE: ['north' | 'west' | 'east' | 'south', number, number][] = [['north', 48, 16], ['west', 16, 48], ['east', 80, 48], ['south', 48, 80]]

function Face({ on, style }: { on: Set<PadButton>; style: PadLayout }) {
  return (
    <svg width="96" height="96" viewBox="0 0 96 96" aria-hidden>
      {FACE.map(([b, cx, cy]) => {
        const g = glyph(style, b)
        return (
          <g key={b}>
            <circle className={`key${on.has(b) ? ' on' : ''}`} cx={cx} cy={cy} r="13" />
            {g.kind === 'letter' && <text className="lbl" x={cx} y={cy + 4} textAnchor="middle">{g.text}</text>}
          </g>
        )
      })}
    </svg>
  )
}

function TestCard({ p, style }: { p: PadInfo; style: PadLayout }) {
  const [s, setS] = useState<PadStateDto | null>(null)
  useEffect(() => {
    let off: (() => void) | undefined
    let live = true
    void events.controllerState((st) => { if (st.id === p.id) setS(st) }).then((u) => { if (live) off = u; else u() })
    void api.controllerTestStart(p.id)
    return () => {
      live = false
      off?.()
      void api.controllerTestStop()
    }
  }, [p.id])
  const on = new Set(s?.buttons ?? [])
  const pct = (v: number | undefined) => Math.round((v ?? 0) * 100)
  const [l2, r2] = TRIGGERS[style]
  const rumble = () => void api.controllerRumble(p.id).then((ok) => { if (!ok) toast("This controller can't rumble here") })
  return (
    <Group title="Test" className="pad-test">
      <div className="pad-test-head">
        <b>{p.name}</b>
        {p.rumble && <Btn onClick={rumble}>Rumble</Btn>}
      </div>
      <div className="pad-trig">
        {([[l2, s?.lt], [r2, s?.rt]] as const).map(([name, v]) => (
          <div key={name}>
            <span className="m">{name}</span>
            <div className="bar"><i style={{ width: `${pct(v)}%` }} /></div>
            <span className="m v">{pct(v)}%</span>
          </div>
        ))}
      </div>
      <div className="pad-pads">
        <Stick x={s?.lx ?? 0} y={s?.ly ?? 0} press={on.has('ls')} />
        <Dpad on={on} />
        <Face on={on} style={style} />
        <Stick x={s?.rx ?? 0} y={s?.ry ?? 0} press={on.has('rs')} />
      </div>
      <div className="pad-chips">
        {CHIPS[style].map(([b, label]) => <span key={b} className={on.has(b) ? 'on' : ''}>{label}</span>)}
      </div>
    </Group>
  )
}

const openSteam = () => void api.steamControllerSettings().catch((e) => toast(String(e)))

/** Settings › Controllers. */
export function ControllersPane() {
  const s = useSettings((x) => x)
  const linux = useLinux()
  const list = usePads((x) => x.list)
  const loaded = usePads((x) => x.loaded)
  const [testing, setTesting] = useState<string | null>(null)
  useEffect(() => {
    startPads()
    void refreshPads()
    // The battery, now and then while the pane is open.
    const t = window.setInterval(() => void refreshPads(), 30_000)
    return () => clearInterval(t)
  }, [])
  const tested = list.find((p) => p.id === testing)
  return (
    <Pane title="Controllers">
      <Group title="Connected">
        {list.map((p) => <DevRow key={p.id} p={p} testing={p.id === testing} onTest={() => setTesting(p.id === testing ? null : p.id)} />)}
        {loaded && list.length === 0 && (
          <Match label="controller connected">
            <div className="set-row pad-dev empty"><div className="l"><PadIcon /><b>No controller connected</b></div></div>
          </Match>
        )}
      </Group>
      <Group title="In grapple">
        <Row label="Navigate with a controller" keywords="pad gamepad focus">
          <Switch on={s.pad_nav} onChange={(v) => void updateSettings({ pad_nav: v })} />
        </Row>
        <Row label="Button prompts" keywords="glyphs icons playstation xbox nintendo">
          <Seg
            value={s.pad_prompts}
            options={[['auto', 'Auto'], ['playstation', 'PlayStation'], ['xbox', 'Xbox'], ['nintendo', 'Nintendo']]}
            onChange={(v) => void updateSettings({ pad_prompts: v })}
          />
        </Row>
        {/* Only where a connected pad can rumble, and only while the pad navigates. */}
        {s.pad_nav && list.some((p) => p.rumble) && (
          <Row label="Vibrate on focus" keywords="rumble">
            <Switch on={s.pad_vibrate} onChange={(v) => void updateSettings({ pad_vibrate: v })} />
          </Row>
        )}
      </Group>
      {linux && (
        <Group title="In games" os="Linux">
          <Row label="Steam games" sub="Steam Input" keywords="controller">
            <Btn onClick={openSteam}>Open Steam's settings</Btn>
          </Row>
          <Row label="Epic and other games" keywords="proton controller xinput dualsense">
            <Choice value={s.pad_in_games} options={IN_GAMES} onChange={(v) => void updateSettings({ pad_in_games: v })} />
          </Row>
        </Group>
      )}
      {tested && <TestCard p={tested} style={promptStyle(s.pad_prompts, tested.layout)} />}
    </Pane>
  )
}

/** The Controller tab of a game's settings sheet. */
export function GameControllerTab({ g }: { g: Game }) {
  const linux = useLinux()
  const def = useSettings((x) => x.pad_in_games)
  const [proton, setProton] = useState<boolean | null>(null)
  const [gs, setGs] = useState<GameSettings | null>(null)
  useEffect(() => {
    api.gameUsesProton(g.id).then(setProton, () => setProton(false))
    api.getGameSettings(g.id).then(setGs, () => setGs(null))
  }, [g.id])
  if (g.store === 'steam') {
    return (
      <Group title="This game" os="Steam">
        <Row label="Controller" sub="Steam Input">
          <Btn onClick={openSteam}>Open Steam's settings</Btn>
        </Row>
      </Group>
    )
  }
  if (proton === null || (linux && !gs)) return null
  if (!linux || !proton || !gs) {
    return (
      <Group title="This game">
        <div className="set-row pad-dev empty"><div className="l"><PadIcon /><b>Gets the controller as is</b></div></div>
      </Group>
    )
  }
  const defLabel = IN_GAMES.find(([v]) => v === def)?.[1] ?? 'Controller as is'
  const set = async (v: PadInGames | null) => {
    // Read the game's settings again first, so a change made on another tab meanwhile is kept.
    const cur = await api.getGameSettings(g.id).catch(() => gs)
    const next = { ...cur, pad_in_games: v }
    setGs(next)
    api.setGameSettings(g.id, next).catch((e) => toast(String(e)))
  }
  return (
    <Group title="This game" os={g.store === 'epic' ? 'Epic · Proton' : 'Proton'}>
      <Row label="Controller" keywords="pad xinput dualsense">
        <Choice<PadInGames | null>
          value={gs.pad_in_games ?? null}
          options={[[null, `Default · ${defLabel}`], ...IN_GAMES]}
          onChange={(v) => void set(v)}
        />
      </Row>
    </Group>
  )
}
