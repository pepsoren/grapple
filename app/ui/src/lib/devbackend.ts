// Browser-only stand-in for the Rust backend (vite dev in a normal browser, screenshots). Not part of the
// production bundle: ipc.ts only imports it when import.meta.env.DEV. The Tauri app uses
// app/src-tauri/src/backend/ instead.
import collection from '../../../../mockups/assets/collection.json'
import { defaultSettings } from './settings'
import type { Game, GameId, GameSettings, LaunchEvent, PadButton, PadInfo, PadStateDto, PlayStats, Settings } from './types'

type Handler = (p: unknown) => void
const handlers = new Map<string, Set<Handler>>()
export function listen(event: string, cb: Handler): () => void {
  if (!handlers.has(event)) handlers.set(event, new Set())
  handlers.get(event)!.add(cb)
  return () => handlers.get(event)?.delete(cb)
}
const emit = (event: string, p: unknown = null) => handlers.get(event)?.forEach((f) => f(p))

interface Row { id: string; n: string; i: boolean; pt: number; lp: number | null; ic: boolean; cv: boolean }
const rows: Row[] = [
  ...(collection as Row[]),
  { id: 'sugar', n: 'Rocket League', i: true, pt: 2480, lp: 1790960400, ic: true, cv: true },
  // Epic, owned, not installed (mockup 07).
  { id: 'fallguys', n: 'Fall Guys', i: false, pt: 0, lp: null, ic: false, cv: true },
]
const FEATURED = new Set(['1144200', '730', '2627570', 'sugar', '275850', '1682970', '4656000', '1478500'])
// Games with wide art in mockups/assets/dl (mockup 07).
const DL_ART = new Set(['252490', '892970', '2399830', '291550', '1063730', 'fallguys'])
const favs = new Set<GameId>(['steam:730', 'steam:1144200', 'steam:252490', 'steam:892970'])

const fold = (s: string) => s.toLowerCase().replace(/^(the|a|an) /, '').replace(/[^a-z0-9 ]/g, '')
const games: Game[] = rows.map((r) => {
  const epic = r.id === 'sugar' || r.id === 'fallguys'
  const id = epic ? `epic:${r.id}` : `steam:${r.id}`
  return {
    id, store: epic ? 'epic' : 'steam', name: r.n, sort_name: fold(r.n), installed: r.i, size_bytes: null,
    art: { cover: r.cv, hero: FEATURED.has(r.id) || DL_ART.has(r.id), logo: FEATURED.has(r.id) && !epic, icon: r.ic },
    art_v: 1, playtime_min: r.pt, last_played: r.lp, status: r.id === '1478500' ? { kind: 'update_queued' } : null,
    favourite: favs.has(id), hidden: false,
  }
})

function stats(id: GameId): PlayStats {
  const g = games.find((x) => x.id === id)!
  const ses = Math.max(1, Math.round(g.playtime_min / 70))
  const days = Array.from({ length: 14 }, (_, i) => {
    const on = ((ses * 7 + i * 13) % 9) > 5
    return on ? (((ses + i * 5) % 7) + 2) * 12 : 0
  })
  return {
    total_min: g.playtime_min, sessions: ses, longest_min: Math.min(g.playtime_min, 60 + (ses % 7) * 25),
    this_week_min: days.slice(7).reduce((a, b) => a + b, 0), last_14_days: days,
  }
}

let settings: Settings = { ...defaultSettings }
const gs = new Map<GameId, GameSettings>()
const GS: GameSettings = { gamemode: null, mangohud: null, close_steam_on_quit: null, show_in_tether: true, proton: null }
let lastScan = Math.floor(Date.now() / 1000) - 120
const hides = (g: Game) => settings.hidden_games.includes(g.id) || settings.hidden_stores.includes(g.store) || (!settings.show_uninstalled && !g.installed)
const PADS: PadInfo[] = [
  { id: '054c:0ce6:mock', name: 'DualSense Wireless Controller', connection: 'usb', battery: null, charging: false, player: 1, layout: 'playstation', rumble: true },
  { id: '045e:0b13:mock', name: 'Xbox Wireless Controller', connection: 'bluetooth', battery: 72, charging: false, player: 2, layout: 'xbox', rumble: true },
]
let padTimer = 0
function padTest(id: string | null) {
  clearInterval(padTimer)
  if (!id) return
  const t0 = performance.now()
  const cycle: PadButton[] = ['south', 'east', 'north', 'west', 'up', 'right', 'down', 'left']
  padTimer = window.setInterval(() => {
    const t = (performance.now() - t0) / 1000
    emit('controller-state', {
      id, buttons: [cycle[Math.floor(t / 0.6) % cycle.length]],
      lx: Math.cos(t * 1.7) * 0.8, ly: Math.sin(t * 1.7) * 0.8, rx: Math.sin(t * 0.9) * 0.5, ry: 0,
      lt: (Math.sin(t * 1.3) + 1) / 2, rt: (Math.cos(t * 0.7) + 1) / 4,
    } satisfies PadStateDto)
  }, 33)
}
let running: [GameId, number] | null = null
let timers: number[] = []

function launch(id: GameId) {
  const g = games.find((x) => x.id === id)!
  emit('launch-started', { game: id })
  const labels = g.store === 'epic' ? ['Epic sign-in token', 'Proton ready', 'Starting game'] : ['Fetching game from Steam', 'Game files ready', 'Starting game']
  const send = (e: LaunchEvent) => emit('launch-event', e)
  labels.forEach((label, i) => {
    timers.push(window.setTimeout(() => {
      if (i > 0) send({ kind: 'step', label: labels[i - 1], done: true })
      send({ kind: 'step', label, done: false })
    }, 350 * i + 150))
  })
  timers.push(window.setTimeout(() => {
    send({ kind: 'step', label: labels[labels.length - 1], done: true })
    running = [id, Math.floor(Date.now() / 1000)]
    send({ kind: 'running', game: id, started_at: running[1] })
  }, 350 * labels.length + 400))
}

const DL_CMDS = new Set(['downloads_snapshot', 'install_plan', 'install_start', 'download_pause', 'download_resume', 'download_cancel', 'download_start_now',
  'download_reorder', 'downloads_pause_all', 'update_now', 'check_update', 'uninstall_game', 'download_settings_get', 'download_settings_set', 'steam_bridge_set'])

export async function invoke(cmd: string, args: Record<string, unknown> = {}): Promise<unknown> {
  if (cmd.startsWith('store_')) return (await import('./store-mock')).storeInvoke(cmd, args)
  if (cmd.startsWith('invite')) {
    const m = await import('./invite-mock')
    m.startInviteMock(emit, () => games)
    return m.invitesInvoke(cmd, args, launch)
  }
  if (DL_CMDS.has(cmd)) {
    const m = await import('./download-mock')
    m.startDownloadMock(emit)
    return m.downloadsInvoke(cmd, args, (id) => games.find((g) => g.id === id)?.name ?? id)
  }
  switch (cmd) {
    case 'list_games': return games.map((g) => ({ ...g, favourite: favs.has(g.id), hidden: hides(g) }))
    case 'game_stats': return stats(args.id as GameId)
    case 'library_stats': {
      const inst = games.filter((g) => g.installed)
      const top = [...games].sort((a, b) => b.playtime_min - a.playtime_min)[0]
      return {
        this_week_min: 285, last_14_days_min: 1150, all_time_min: inst.reduce((a, g) => a + g.playtime_min, 0), most_played: top.id,
      }
    }
    case 'launch': launch(args.id as GameId); return
    case 'stop': {
      timers.forEach(clearTimeout); timers = []
      if (running) emit('launch-event', { kind: 'exited', game: running[0], ended_at: Math.floor(Date.now() / 1000) })
      running = null
      return
    }
    case 'running': return running ? [running] : []
    case 'set_favourite': { const id = args.id as GameId; if (args.on) favs.add(id); else favs.delete(id); return }
    case 'get_settings': return settings
    case 'set_settings': {
      const before = settings
      settings = args.settings as Settings
      if (before.hidden_games !== settings.hidden_games || before.hidden_stores !== settings.hidden_stores || before.show_uninstalled !== settings.show_uninstalled) {
        setTimeout(() => emit('games-changed'), 0)
      }
      return settings
    }
    case 'settings_reset': settings = { ...defaultSettings }; setTimeout(() => emit('games-changed'), 0); return settings
    case 'rescan': lastScan = Math.floor(Date.now() / 1000); emit('games-changed'); return
    case 'platform': return { os: 'linux', version: '0.1.0' }
    case 'hide_main': return
    case 'library_info': {
      const count = (s: Game['store']) => ({ store: s, games: games.filter((g) => g.store === s).length, installed: games.filter((g) => g.store === s && g.installed).length })
      return {
        sources: [count('steam'), count('epic'), count('manual')],
        folders: [
          { path: '~/.local/share/Steam', store: 'steam', installed: 1, free_bytes: 3.0e9 },
          { path: '/data/SteamLibrary', store: 'steam', installed: games.filter((g) => g.store === 'steam' && g.installed).length - 1, free_bytes: 552e9 },
          { path: '~/Games/Heroic', store: 'epic', installed: games.filter((g) => g.store === 'epic' && g.installed).length, free_bytes: 3.0e9 },
        ],
        last_scan: lastScan,
        hidden: settings.hidden_games.flatMap((id) => games.filter((g) => g.id === id).map((g) => ({ id, name: g.name }))),
      }
    }
    case 'protons': return {
      choices: ['GE-Proton (latest)', 'GE-Proton10-25', 'Proton-CachyOS', 'Proton - Experimental'],
      versions: [{ name: 'GE-Proton10-25', source: 'heroic' }, { name: 'Proton-CachyOS', source: 'heroic' }, { name: 'Proton - Experimental', source: 'steam' }],
      heroic_default: 'GE-Proton10-25',
    }
    case 'accounts': return { steam_found: true, steam_name: 'player1', store_data: true, epic_signed_in: true, epic_name: 'Josh' }
    case 'store_sign_out': return
    case 'game_files': {
      const g = games.find((x) => x.id === args.id)
      return g?.installed ? { folder: `~/Games/${g.name}`, size_bytes: 43.7e9 } : { folder: null, size_bytes: null }
    }
    case 'games_not_shared': return [...gs].filter(([, v]) => !v.show_in_tether).map(([k]) => k)
    case 'storage_info': return { cache_path: '~/.cache/grapple', cache_bytes: 32e6, data_path: '~/.local/share/grapple', data_bytes: 5.5e6, logs_path: '~/.local/state/grapple/logs', has_log: true }
    case 'cache_clear': return 32e6
    case 'open_place': return
    case 'last_launch_log': return 'browser mock: no game ran\n'
    case 'theme_file': return { path: '~/.config/grapple/theme.css', exists: false }
    case 'theme_open': return
    // Controllers: two sample pads; the tested one moves by itself (src-tauri/src/controllers/mock.rs).
    case 'controllers_list': return PADS
    case 'controller_test_start': padTest(String(args.id)); return
    case 'controller_test_stop': padTest(null); return
    case 'controller_rumble': return true
    case 'steam_controller_settings': return
    case 'game_uses_proton': return String(args.id).startsWith('epic:')
    case 'eos_overlay_set': {
      await new Promise((r) => setTimeout(r, 800))
      const id = args.game as GameId
      gs.set(id, { ...(gs.get(id) ?? GS), eos_overlay: Boolean(args.on) })
      return
    }
    case 'get_game_settings': return gs.get(args.id as GameId) ?? GS
    case 'set_game_settings': gs.set(args.id as GameId, args.settings as GameSettings); return
    case 'get_theme_override': return ''
    case 'art_custom': return []
    case 'art_pick': return false
    case 'art_reset': return
    case 'install_game': {
      const m = await import('./download-mock')
      const g = games.find((x) => x.id === args.id)
      const t = m.plan(String(args.id)).targets.find((x) => x.isDefault)
      return m.downloadsInvoke('install_start', { game: args.id, targetDir: t?.path }, () => g?.name ?? String(args.id))
    }
    case 'pick_program': return { name: 'Hollow Knight', path: '/games/Hollow_Knight/hollow_knight.x86_64', kind: 'native', label: 'Native program' }
    case 'detect_program': {
      const path = String(args.path)
      const low = path.toLowerCase()
      const kind = /\.(exe|bat|cmd|msi)$/.test(low) ? 'windows' : low.endsWith('.appimage') ? 'appimage' : path.includes('/') ? 'native' : 'command'
      const label = { windows: 'Windows program, runs through Proton', appimage: 'AppImage', native: 'Native program', command: 'Custom command' }[kind]
      return { name: (path.split('/').pop() ?? path).replace(/\.[A-Za-z0-9]+$/, '').replace(/_/g, ' '), path, kind, label }
    }
    case 'add_manual_game': {
      const d = args.draft as { name: string; path: string }
      if (!d.name.trim()) throw 'Give the game a name'
      const id = `manual:${Math.random().toString(16).slice(2, 10)}`
      games.push({
        id, store: 'manual', name: d.name.trim(), sort_name: fold(d.name), installed: true, size_bytes: null,
        art: { cover: false, hero: false, logo: false, icon: false }, art_v: 1, playtime_min: 0, last_played: null, status: null, favourite: false, hidden: false,
      })
      emit('games-changed')
      return id
    }
    case 'remove_manual_game': {
      const i = games.findIndex((x) => x.id === args.id)
      if (i >= 0) games.splice(i, 1)
      emit('games-changed')
      return
    }
    // The browser has no real tether: the UI falls back to its fixture friends.
    case 'tether_status': case 'tether_sign_in': case 'tether_sign_out': case 'tether_set_instance_url':
      return { mock: true, loaded: true, signed_in: false, signing_in: false, connection: 'off', error: null, instance_url: 'mock', me: null, friends: [] }
    default: return
  }
}
