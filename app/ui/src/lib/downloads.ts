// Downloads in the UI: the snapshot Rust pushes (`downloads-changed`, at most 4 a second), the install sheet, and the
// thin actions. The UI never polls: it renders what arrives.
import type { DlItem, DlSettings, DlSnapshot, InstallPlan } from './download-types'
import { api, events } from './ipc'
import { createStore, useStore } from './store'
import type { Game, GameId } from './types'
import { toast } from './ui'

interface Downloads {
  snap: DlSnapshot | null
  /** The install sheet: the game it is for, and its plan once Rust has it. */
  sheet: { game: Game; plan: InstallPlan | null; error: string | null } | null
  settings: DlSettings | null
}

const empty: DlSnapshot = { queued: [], updates: [], done: [], pausedAll: false, steamBridge: 'off' }
export const downloadsStore = createStore<Downloads>({ snap: null, sheet: null, settings: null })
export const useDownloads = <S>(select: (s: Downloads) => S) => useStore(downloadsStore, select)

let started = false
/** Listen for pushes and take the first snapshot. Called once at start-up. */
export function startDownloads() {
  if (started) return
  started = true
  void events.downloadsChanged((snap) => downloadsStore.set({ snap }))
  void refreshDownloads()
}

/** One snapshot on demand (start-up, opening the page). */
export async function refreshDownloads() {
  try {
    downloadsStore.set({ snap: await api.downloadsSnapshot() })
  } catch {
    downloadsStore.set({ snap: empty })
  }
}

const run = (p: Promise<unknown>) => p.catch((e) => toast(String(e)))
export const pauseItem = (g: GameId) => run(api.downloadPause(g))
export const resumeItem = (g: GameId) => run(api.downloadResume(g))
export const cancelItem = (g: GameId) => run(api.downloadCancel(g))
export const startNow = (g: GameId) => run(api.downloadStartNow(g))
export const updateNow = (g: GameId) => run(api.updateNow(g))
export const pauseAll = (paused: boolean) => run(api.downloadsPauseAll(paused))
export const reorder = (games: GameId[]) => run(api.downloadReorder(games))

/** Install: open the sheet; Rust works out the size and the drives. */
export function installGame(game: Game) {
  downloadsStore.set({ sheet: { game, plan: null, error: null } })
  api.installPlan(game.id).then(
    (plan) => { if (downloadsStore.get().sheet?.game.id === game.id) downloadsStore.set({ sheet: { game, plan, error: null } }) },
    (e) => { if (downloadsStore.get().sheet?.game.id === game.id) downloadsStore.set({ sheet: { game, plan: null, error: String(e) } }) },
  )
}
export const closeSheet = () => downloadsStore.set({ sheet: null })

export async function confirmInstall(game: Game, targetDir: string, proton: string | null) {
  try {
    await api.installStart(game.id, targetDir, proton)
    closeSheet()
    toast(`${game.name} is in the queue`)
  } catch (e) {
    toast(String(e))
  }
}

export async function loadDownloadSettings() {
  try {
    downloadsStore.set({ settings: await api.downloadSettingsGet() })
  } catch (e) {
    toast(String(e))
  }
}

export async function updateDownloadSettings(patch: Partial<DlSettings>) {
  const cur = downloadsStore.get().settings
  if (!cur) return
  const next = { ...cur, ...patch }
  downloadsStore.set({ settings: next })
  try {
    downloadsStore.set({ settings: await api.downloadSettingsSet(next) })
  } catch (e) {
    downloadsStore.set({ settings: cur })
    toast(String(e))
  }
}

export async function setSteamBridge(on: boolean) {
  try {
    downloadsStore.set({ settings: await api.steamBridgeSet(on) })
  } catch (e) {
    toast(String(e))
  }
}

// ---------------------------------------------------------------------------------------------- formatting

/** "37.6 GB", "1.34 GB", "553 GB", "940 MB". */
export function size(bytes: number | null | undefined): string {
  if (bytes == null) return 'Checking size'
  if (bytes >= 100e9) return `${Math.round(bytes / 1e9)} GB`
  if (bytes >= 1e9) return `${parseFloat((bytes / 1e9).toPrecision(3))} GB`
  if (bytes >= 1e6) return `${Math.round(bytes / 1e6)} MB`
  return `${Math.max(1, Math.round(bytes / 1e3))} KB`
}

/** "14.2 of 37.6 GB". */
export function ofTotal(done: number, total: number | null): string {
  if (total == null) return size(done)
  const unit = total >= 1e9 ? 1e9 : 1e6
  const u = unit === 1e9 ? 'GB' : 'MB'
  return `${(done / unit).toFixed(unit === 1e9 ? 1 : 0)} of ${(total / unit).toFixed(unit === 1e9 ? 1 : 0)} ${u}`
}

/** "42.8 MB/s". */
export function speed(bps: number | null | undefined): string {
  if (!bps) return '—'
  if (bps >= 1e6) return `${(bps / 1e6).toFixed(1)} MB/s`
  return `${Math.round(bps / 1e3)} KB/s`
}

/** "9 min", "1 h 05 min", "< 1 min". */
export function left(sec: number | null | undefined): string {
  if (sec == null) return '—'
  if (sec < 60) return '< 1 min'
  const m = Math.round(sec / 60)
  if (m < 60) return `${m} min`
  return `${Math.floor(m / 60)} h ${String(m % 60).padStart(2, '0')} min`
}

/** "12 min ago". */
export function since(t: number | undefined): string {
  if (!t) return ''
  const m = Math.max(0, Math.round((Date.now() / 1000 - t) / 60))
  if (m < 1) return 'just now'
  if (m < 60) return `${m} min ago`
  const h = Math.round(m / 60)
  return `${h} h ago`
}

export const STATE_WORD: Record<DlItem['state'], string> = {
  queued: 'Queued', starting: 'Starting', downloading: 'Downloading', verifying: 'Verifying', installing: 'Installing',
  paused: 'Paused', done: 'Done', error: 'Error',
}

export const progress = (i: DlItem) => (i.bytesTotal ? Math.min(1, i.bytesDone / i.bytesTotal) : 0)

/** Overall progress of what is active or queued (the top-bar ring). */
export function overall(s: DlSnapshot): number {
  const items = [s.active, ...s.queued].filter((x): x is DlItem => !!x)
  const total = items.reduce((a, i) => a + (i.bytesTotal ?? 0), 0)
  const done = items.reduce((a, i) => a + (i.bytesTotal ? Math.min(i.bytesDone, i.bytesTotal) : 0), 0)
  return total ? done / total : 0
}

export const artOf = (i: DlItem) => i.art.header ?? i.art.hero ?? i.art.cover
