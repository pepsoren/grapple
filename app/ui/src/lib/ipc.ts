// The only file that talks to Tauri. In a plain browser (vite dev, screenshots) it falls back to
// src/lib/devbackend.ts, which is loaded lazily and never ships in the app bundle's critical path.
import type { DlSettings, DlSnapshot, InstallPlan } from './download-types'
import type {
  AccountsInfo, ArtKind, Detected, Game, GameFiles, GameId, GameSettings, InvitesSnapshot, LaunchEvent, LaunchStarted, LibraryInfo,
  LibraryStats, ManualDraft, PadInfo, PadNav, PadStateDto, PlatformInfo, PlayOutcome, PlayStats, ProtonsInfo, SendResult, Settings,
  StorageInfo, TetherSnapshot,
} from './types'

export const inTauri = typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window

type Unlisten = () => void
type Dev = typeof import('./devbackend')

let devp: Promise<Dev> | null = null
const dev = () =>
  (devp ??= import.meta.env.DEV ? import('./devbackend') : Promise.reject(new Error('Tauri is not available')))

export async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (inTauri) {
    const { invoke } = await import('@tauri-apps/api/core')
    return invoke<T>(cmd, args)
  }
  return (await dev()).invoke(cmd, args) as Promise<T>
}

export async function listen<T>(event: string, cb: (payload: T) => void): Promise<Unlisten> {
  if (inTauri) {
    const { listen } = await import('@tauri-apps/api/event')
    return listen<T>(event, (e) => cb(e.payload))
  }
  return (await dev()).listen(event, cb as (p: unknown) => void)
}

export const api = {
  listGames: () => call<Game[]>('list_games'),
  gameStats: (id: GameId) => call<PlayStats>('game_stats', { id }),
  libraryStats: () => call<LibraryStats>('library_stats'),
  launch: (id: GameId) => call<void>('launch', { id }),
  stop: (id: GameId) => call<void>('stop', { id }),
  setFavourite: (id: GameId, on: boolean) => call<void>('set_favourite', { id, on }),
  getSettings: () => call<Settings>('get_settings'),
  setSettings: (settings: Settings) => call<Settings>('set_settings', { settings }),
  getGameSettings: (id: GameId) => call<GameSettings>('get_game_settings', { id }),
  setGameSettings: (id: GameId, settings: GameSettings) => call<void>('set_game_settings', { id, settings }),
  eosOverlaySet: (game: GameId, on: boolean) => call<void>('eos_overlay_set', { game, on }),
  rescan: () => call<void>('rescan'),
  /** Old Install: an alias of install_start into the default drive (the UI uses the install sheet instead). */
  installGame: (id: GameId) => call<void>('install_game', { id }),
  downloadsSnapshot: () => call<DlSnapshot>('downloads_snapshot'),
  installPlan: (game: GameId) => call<InstallPlan>('install_plan', { game }),
  installStart: (game: GameId, targetDir: string, proton?: string | null) => call<void>('install_start', { game, targetDir, proton: proton ?? null }),
  downloadPause: (game: GameId) => call<void>('download_pause', { game }),
  downloadResume: (game: GameId) => call<void>('download_resume', { game }),
  downloadCancel: (game: GameId) => call<void>('download_cancel', { game }),
  downloadStartNow: (game: GameId) => call<void>('download_start_now', { game }),
  downloadReorder: (games: GameId[]) => call<void>('download_reorder', { games }),
  downloadsPauseAll: (paused: boolean) => call<void>('downloads_pause_all', { paused }),
  updateNow: (game: GameId) => call<void>('update_now', { game }),
  artCustom: (game: GameId) => call<ArtKind[]>('art_custom', { game }),
  artPick: (game: GameId, kind: ArtKind) => call<boolean>('art_pick', { game, kind }),
  artReset: (game: GameId, kind: ArtKind) => call<void>('art_reset', { game, kind }),
  checkUpdate: (game: GameId) => call<'upToDate' | 'available' | 'queued' | 'downloading'>('check_update', { game }),
  uninstallGame: (game: GameId) => call<void>('uninstall_game', { game }),
  downloadSettingsGet: () => call<DlSettings>('download_settings_get'),
  downloadSettingsSet: (settings: DlSettings) => call<DlSettings>('download_settings_set', { settings }),
  steamBridgeSet: (on: boolean) => call<DlSettings>('steam_bridge_set', { on }),
  /** The file picker; null when cancelled. */
  pickProgram: () => call<Detected | null>('pick_program'),
  detectProgram: (path: string) => call<Detected>('detect_program', { path }),
  addManualGame: (draft: ManualDraft) => call<GameId>('add_manual_game', { draft }),
  removeManualGame: (id: GameId) => call<void>('remove_manual_game', { id }),
  tetherStatus: () => call<TetherSnapshot>('tether_status'),
  /** Opens the browser and waits for the answer: only from the sign-in button. */
  tetherSignIn: () => call<TetherSnapshot>('tether_sign_in'),
  tetherCancelSignIn: () => call<void>('tether_cancel_sign_in'),
  tetherSignOut: () => call<TetherSnapshot>('tether_sign_out'),
  tetherSetInstanceUrl: (url: string) => call<TetherSnapshot>('tether_set_instance_url', { url }),
  // Game invites (src-tauri/src/invites.rs)
  invitesSnapshot: () => call<InvitesSnapshot>('invites_snapshot'),
  /** One invite per friend (tether user ids), open for 15 minutes. */
  inviteSend: (game: GameId, friends: string[]) => call<SendResult[]>('invite_send', { game, friends }),
  inviteCancel: (id: string) => call<void>('invite_cancel', { id }),
  inviteDecline: (id: string) => call<void>('invite_decline', { id }),
  invitesSeen: (ids: string[]) => call<void>('invites_seen', { ids }),
  /** Play on an invite to Josh: only from a Play button the person pressed. */
  inviteAccept: (id: string) => call<PlayOutcome>('invite_accept', { id }),
  inviteClose: () => call<void>('invite_close'),
  inviteSettingsGet: () => call<'friends' | 'nobody'>('invite_settings_get'),
  inviteSettingsSet: (from: 'friends' | 'nobody') => call<'friends' | 'nobody'>('invite_settings_set', { from }),
  getThemeOverride: () => call<string>('get_theme_override'),
  /** What is running right now: [game id, started_at]. */
  running: () => call<[GameId, number][]>('running'),
  /** Shrink to the 380x92 playing card (true) or restore the saved size (false). */
  setCompact: (on: boolean) => call<void>('set_compact', { on }),
  log: (msg: string) => call<void>('ui_log', { msg }),
  uiReady: () => call<void>('ui_ready'),
  shown: () => call<void>('report_shown'),
  quit: () => call<void>('quit_app'),
  showMain: () => call<void>('show_main'),
  /** The window's own background (the canvas colour), painted under the page. */
  setWindowBg: (r: number, g: number, b: number) => call<void>('set_window_bg', { r, g, b }),
  // Settings page
  platform: () => call<PlatformInfo>('platform'),
  hideMain: () => call<void>('hide_main'),
  libraryInfo: () => call<LibraryInfo>('library_info'),
  protons: () => call<ProtonsInfo>('protons'),
  accounts: () => call<AccountsInfo>('accounts'),
  storeSignOut: () => call<void>('store_sign_out'),
  gameFiles: (id: GameId) => call<GameFiles>('game_files', { id }),
  gamesNotShared: () => call<GameId[]>('games_not_shared'),
  storageInfo: () => call<StorageInfo>('storage_info'),
  cacheClear: () => call<number>('cache_clear'),
  /** 'data' | 'logs' | 'game' (with the game's id). */
  openPlace: (place: 'data' | 'logs' | 'game', game?: GameId) => call<void>('open_place', { place, game: game ?? null }),
  lastLaunchLog: () => call<string>('last_launch_log'),
  themeFile: () => call<{ path: string; exists: boolean }>('theme_file'),
  themeOpen: () => call<void>('theme_open'),
  settingsReset: () => call<Settings>('settings_reset'),
  // Controllers
  controllersList: () => call<PadInfo[]>('controllers_list'),
  /** Streams controller-state for this pad until controllerTestStop. */
  controllerTestStart: (id: string) => call<void>('controller_test_start', { id }),
  controllerTestStop: () => call<void>('controller_test_stop'),
  /** false: the pad cannot rumble. */
  controllerRumble: (id: string, kind: 'test' | 'tick' = 'test') => call<boolean>('controller_rumble', { id, kind }),
  /** Steam's own controller settings (a Steam window). */
  steamControllerSettings: () => call<void>('steam_controller_settings'),
  /** The game runs through Proton (Epic, Windows programs added by hand): its Controller choice applies. */
  gameUsesProton: (id: GameId) => call<boolean>('game_uses_proton', { id }),
}

export const events = {
  controllersChanged: (cb: (pads: PadInfo[]) => void) => listen<PadInfo[]>('controllers-changed', cb),
  controllerState: (cb: (s: PadStateDto) => void) => listen<PadStateDto>('controller-state', cb),
  padNav: (cb: (n: PadNav) => void) => listen<PadNav>('pad-nav', cb),
  gamesChanged: (cb: () => void) => listen<null>('games-changed', cb),
  launchEvent: (cb: (e: LaunchEvent) => void) => listen<LaunchEvent>('launch-event', cb),
  launchStarted: (cb: (e: LaunchStarted) => void) => listen<LaunchStarted>('launch-started', cb),
  tetherChanged: (cb: (s: TetherSnapshot) => void) => listen<TetherSnapshot>('tether-changed', cb),
  downloadsChanged: (cb: (s: DlSnapshot) => void) => listen<DlSnapshot>('downloads-changed', cb),
  invitesChanged: (cb: (s: InvitesSnapshot) => void) => listen<InvitesSnapshot>('invites-changed', cb),
  /** Play pressed on an invite's card in tether: grapple already launched an installed game, else this opens. */
  invitePlay: (cb: (o: PlayOutcome) => void) => listen<PlayOutcome>('invite-play', cb),
}

/** grapple://art/<game-id>/<kind>?v=<art_v>. Linux/macOS use grapple://, Windows http://grapple.localhost. */
export function artUrl(g: Pick<Game, 'id' | 'art_v'>, kind: ArtKind): string {
  const id = encodeURIComponent(g.id)
  if (!inTauri) return `/@dev-art/${id}/${kind}?v=${g.art_v}`
  const win = navigator.userAgent.includes('Windows')
  return `${win ? 'http://grapple.localhost' : 'grapple://art'}${win ? '/art' : ''}/${id}/${kind}?v=${g.art_v}`
}
