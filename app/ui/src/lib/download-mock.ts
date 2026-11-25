// Browser-only stand-in for the downloads commands (vite dev, screenshots): the mockup's queue (Rust, Fall Guys,
// Valheim, ARK, Big Walk, Uncrashed, Brawlhalla) that moves on its own, like app/src-tauri/src/backend/mock_downloads.rs.
// MOCK: nothing here downloads or touches a store.
import type { DlItem, DlSettings, DlSnapshot, InstallPlan } from './download-types'

type Emit = (event: string, payload: unknown) => void
const GB = 1e9
const art = (id: string) => {
  const e = encodeURIComponent(id)
  return { hero: `/@dev-art/${e}/hero?v=1`, header: `/@dev-art/${e}/header?v=1`, cover: `/@dev-art/${e}/cover?v=1` }
}

function item(game: string, name: string, store: 'steam' | 'epic', kind: 'install' | 'update', total: number | null, dir: string, position: number, base = 40): DlItem & { base: number } {
  return {
    game, store, kind, state: 'queued', name, art: art(game), bytesDone: 0, bytesTotal: total == null ? null : total * GB, bytesPerSec: null,
    secondsLeft: null, speedHistory: [], targetDir: dir, position, canPause: true, canReorder: true, canCancel: true, base: base * 1e6,
  }
}

const rust = { ...item('steam:252490', 'Rust', 'steam', 'install', 37.6, '/data/SteamLibrary', 0, 42.8), state: 'downloading' as const, bytesDone: 14.2 * GB,
  speedHistory: [30, 34, 41, 38, 44, 40, 46, 42, 39, 43, 45, 41, 44, 47, 42, 40, 43, 46, 44, 42].map((v) => v * 1e6) }
let active: (DlItem & { base: number }) | undefined = rust
let queued = [
  item('epic:fallguys', 'Fall Guys', 'epic', 'install', 4.1, '/data/Games', 1, 31),
  item('steam:892970', 'Valheim', 'steam', 'install', 1.6, '/data/SteamLibrary', 2, 38),
  item('steam:2399830', 'ARK: Survival Ascended', 'steam', 'install', 66.7, '/data/SteamLibrary', 3, 44),
]
let updates = [
  item('steam:1478500', 'Big Walk', 'steam', 'update', 1.34, '~/.local/share/Steam', 4),
  item('steam:1682970', 'Uncrashed : FPV Drone Simulator', 'steam', 'update', null, '/data/SteamLibrary', 5),
]
let done: DlItem[] = [{ ...item('steam:291550', 'Brawlhalla', 'steam', 'install', 18, '/data/SteamLibrary', 0), state: 'done' as const, bytesDone: 18 * GB, finishedAt: Math.floor(Date.now() / 1000) - 12 * 60 }]
let pausedAll = false
let settings: DlSettings = {
  speedLimitBytes: 0, connections: 8, autoUpdate: true, updateOnlyWhenIdle: true, pauseWhilePlaying: true, notifyReady: true,
  steamDefaultDir: '/data/SteamLibrary', epicDefaultDir: '/data/Games', steamBridge: true,
}
const choices = { steam: ['~/.local/share/Steam', '/data/SteamLibrary'], epic: ['~/Games/Heroic', '/data/Games'] }

const strip = (i: DlItem & { base?: number }): DlItem => { const { base: _b, ...rest } = i; return rest }
export function snapshot(): DlSnapshot {
  // ?state=downloads-empty: the empty page.
  if (new URLSearchParams(location.search).get('state') === 'downloads-empty') return { queued: [], updates: [], done: [], pausedAll: false, steamBridge: 'off' }
  return { active: active && strip(active), queued: queued.map(strip), updates: updates.map(strip), done, pausedAll, steamBridge: settings.steamBridge ? 'on' : 'off' }
}

let emit: Emit = () => {}
let tick = 0
let timer = 0
const changed = () => emit('downloads-changed', snapshot())

function promote() {
  if (active || pausedAll) return
  const i = queued.findIndex((x) => x.state === 'queued')
  if (i >= 0) active = { ...queued.splice(i, 1)[0], state: 'downloading' }
}

/** SIMULATED: four steps a second while something downloads (what Rust pushes in the app). */
export function startDownloadMock(e: Emit) {
  emit = e
  if (timer) return
  timer = window.setInterval(() => {
    if (!active || pausedAll || active.state !== 'downloading') return
    tick++
    const wobble = [0, 3, -2, 5, -4, 2, -1, 4][tick % 8] * 1e6
    let sp = Math.max(1, active.base + wobble)
    if (settings.speedLimitBytes) sp = Math.min(sp, settings.speedLimitBytes)
    active.bytesPerSec = sp
    active.bytesDone += sp / 4
    if (tick % 4 === 0) active.speedHistory = [...active.speedHistory, sp].slice(-20)
    const total = active.bytesTotal ?? Infinity
    active.secondsLeft = Math.round((total - active.bytesDone) / sp)
    if (active.bytesDone >= total) {
      done = [{ ...strip(active), state: 'done' as const, bytesDone: total, bytesPerSec: null, finishedAt: Math.floor(Date.now() / 1000) }, ...done].slice(0, 10)
      active = undefined
      promote()
      emit('games-changed', null)
    }
    changed()
  }, 250)
}

export function plan(game: string): InstallPlan {
  const epic = game.startsWith('epic:')
  const sz = game === 'steam:252490' ? 37.6 : game === 'epic:fallguys' ? 4.1 : 2 + (game.length % 7) * 3.3
  const t = (path: string, label: string, free: number, total: number, isDefault: boolean) => ({ path, label, freeBytes: free * GB, totalBytes: total * GB, enough: free >= sz + 1, isDefault })
  return {
    game, store: epic ? 'epic' : 'steam', sizeBytes: sz * GB,
    targets: epic
      ? [t('~/Games/Heroic', 'Home drive', 2.4, 120, false), t('/data/Games', 'Data drive', 553, 780, true)]
      : [t('~/.local/share/Steam', 'Home drive', 2.4, 120, false), t('/data/SteamLibrary', 'Data drive', 553, 780, true)],
    protons: epic ? [{ name: 'GE-Proton (latest)', isDefault: true }, { name: 'GE-Proton10-25', isDefault: false }, { name: 'Proton 9.0 (Beta)', isDefault: false }] : undefined,
  }
}

export function downloadsInvoke(cmd: string, args: Record<string, unknown>, nameOf: (id: string) => string): unknown {
  const g = args.game as string
  const take = (list: typeof queued) => { const i = list.findIndex((x) => x.game === g); return i >= 0 ? list.splice(i, 1)[0] : undefined }
  switch (cmd) {
    case 'downloads_snapshot': return snapshot()
    case 'install_plan': return plan(g)
    case 'install_start': {
      if (active?.game === g || queued.some((x) => x.game === g)) throw 'already in the queue'
      queued.push(item(g, nameOf(g), g.startsWith('epic:') ? 'epic' : 'steam', 'install', 6.5, String(args.targetDir), queued.length + 1, 60))
      promote(); changed(); return
    }
    case 'download_pause':
      if (active?.game === g) { queued.unshift({ ...active, state: 'paused', bytesPerSec: null, secondsLeft: null }); active = undefined; promote() } else { const i = queued.find((x) => x.game === g); if (i) i.state = 'paused' }
      changed(); return
    case 'download_resume': { const i = queued.find((x) => x.game === g); if (i) i.state = 'queued'; promote(); changed(); return }
    case 'check_update': return g.startsWith('epic:') ? 'available' : 'upToDate'
    case 'download_cancel': if (active?.game === g) active = undefined; take(queued); take(updates); promote(); changed(); return
    case 'download_start_now': case 'update_now': {
      const n = take(queued) ?? take(updates)
      if (n) {
        if (active) queued.unshift({ ...active, state: 'queued', bytesPerSec: null, secondsLeft: null })
        active = { ...n, state: 'downloading', bytesTotal: n.bytesTotal ?? 2.1 * GB }
        pausedAll = false
      }
      changed(); return
    }
    case 'download_reorder': {
      const order = args.games as string[]
      queued = [...order.map((id) => queued.find((x) => x.game === id)).filter((x): x is (typeof queued)[number] => !!x), ...queued.filter((x) => !order.includes(x.game))]
      queued.forEach((x, i) => (x.position = i + 1)); changed(); return
    }
    case 'downloads_pause_all':
      pausedAll = !!args.paused
      if (active) { active.state = pausedAll ? 'paused' : 'downloading'; if (pausedAll) { active.bytesPerSec = null; active.secondsLeft = null } }
      changed(); return
    case 'uninstall_game': return
    case 'download_settings_get': return { ...settings, choices }
    case 'download_settings_set': settings = { ...(args.settings as DlSettings), steamBridge: settings.steamBridge }; changed(); return { ...settings, choices }
    case 'steam_bridge_set': {
      settings = { ...settings, steamBridge: !!args.on }
      for (const i of [active, ...queued, ...updates]) if (i && i.store === 'steam') { i.canPause = i.canReorder = i.canCancel = settings.steamBridge }
      changed(); return { ...settings, choices }
    }
    default: return undefined
  }
}
