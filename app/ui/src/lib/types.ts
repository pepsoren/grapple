// Mirrors crates/grapple-core/src/model.rs (serde output) plus the app's own DTOs.
// Change both together; the lead owns the shapes.

export type Store = 'steam' | 'epic' | 'manual'
export type GameId = string // "steam:<appid>" | "epic:<app_name>" | "manual:<uuid>"
export type ArtKind = 'cover' | 'hero' | 'logo' | 'icon'

export type GameStatus = { kind: 'update_queued' } | { kind: 'updating' } | { kind: 'running' }

/** What list_games returns: grapple_core::Game with the art paths replaced by "does it exist" flags
 *  (the files are served over grapple://art/<id>/<kind>), plus favourite and the playtime to show. */
export interface Game {
  id: GameId
  store: Store
  name: string
  sort_name: string
  installed: boolean
  size_bytes: number | null
  art: Record<ArtKind, boolean>
  /** Bumps when any art file changes; part of the art URL so the cache never serves stale art. */
  art_v: number
  /** max(store total, our sessions), minutes. */
  playtime_min: number
  /** Unix seconds. */
  last_played: number | null
  status: GameStatus | null
  favourite: boolean
  /** Left out of the lists by the settings (hidden by hand, store off, not installed); still known when it runs. */
  hidden: boolean
}

/** grapple_core::PlayStats */
export interface PlayStats {
  total_min: number
  sessions: number
  longest_min: number
  this_week_min: number
  last_14_days: number[]
}

/** grapple_core::LaunchEvent (internally tagged by "kind") */
export type LaunchEvent =
  | { kind: 'step'; label: string; done: boolean }
  | { kind: 'running'; game: GameId; started_at: number }
  | { kind: 'exited'; game: GameId; ended_at: number }
  | { kind: 'failed'; game: GameId; message: string }

/** Emitted by the app (not grapple_core) when a launch starts, whoever asked for it. */
export interface LaunchStarted {
  game: GameId
}

export interface Settings {
  /** XDG autostart entry (opt-in). */
  autostart: boolean
  /** Start `steam -silent` when Grapple starts. */
  keep_steam_ready: boolean
  /** Keep Proton's Steam Linux Runtime updated while no game runs, so launches never wait for it. */
  update_runtime: boolean
  /** Global switch for "now playing" in tether. */
  show_in_tether: boolean
  accent: string
  look: string
  // Settings page (mockups/08-settings.html)
  /** The login entry starts grapple hidden in the tray. */
  start_hidden: boolean
  open_on: 'home' | 'library' | 'collection' | 'store'
  /** Closing the window keeps grapple in the tray (false: quits). */
  close_to_tray: boolean
  /** A game grapple started is running: shrink to the playing card, hide grapple, or keep it. */
  on_game_start: 'card' | 'hide' | 'keep'
  on_game_exit: 'show' | 'stay'
  /** Percent. */
  ui_scale: number
  reduce_motion: boolean
  hidden_stores: string[]
  show_uninstalled: boolean
  hidden_games: GameId[]
  week_start: 'monday' | 'sunday'
  /** Close Steam when I quit a game (games without their own choice). */
  close_steam: boolean
  /** Linux: Proton for Epic and other Windows games; null = Heroic's choice. */
  default_proton: string | null
  default_gamemode: boolean
  default_mangohud: boolean
  friends_on_home: boolean
  quiet_while_playing: boolean
  // Controllers (src-tauri/src/controllers/)
  /** A pad moves the focus while grapple is in front. */
  pad_nav: boolean
  pad_prompts: PadPrompts
  /** A short rumble when the pad moves the focus. */
  pad_vibrate: boolean
  /** Linux: how pads are shown to Epic and other Proton games. */
  pad_in_games: PadInGames
}

/** Button prompts: auto picks by the pad's maker. */
export type PadPrompts = 'auto' | 'playstation' | 'xbox' | 'nintendo'
/** as_is: Proton's own choice; xbox: every pad as an Xbox-style (XInput) controller. */
export type PadInGames = 'as_is' | 'xbox'
export type PadLayout = 'playstation' | 'xbox' | 'nintendo'

/** controllers_list / controllers-changed (src-tauri/src/controllers/mod.rs `PadInfo`). */
export interface PadInfo {
  id: string
  name: string
  connection: 'usb' | 'bluetooth' | 'wired' | null
  /** Percent; null when the system does not say. */
  battery: number | null
  charging: boolean
  player: number
  layout: PadLayout
  rumble: boolean
}

export type PadButton =
  | 'south' | 'east' | 'west' | 'north' | 'lb' | 'rb' | 'lt' | 'rt' | 'select' | 'start' | 'guide' | 'ls' | 'rs'
  | 'up' | 'down' | 'left' | 'right'

/** controller-state: the tested pad. Sticks -1..1 (y down), triggers 0..1. */
export interface PadStateDto {
  id: string
  buttons: PadButton[]
  lx: number
  ly: number
  rx: number
  ry: number
  lt: number
  rt: number
}

export type PadAction = 'up' | 'down' | 'left' | 'right' | 'confirm' | 'back' | 'alt' | 'search' | 'menu' | 'prev' | 'next'

/** pad-nav */
export interface PadNav {
  action: PadAction
  pad: string
  layout: PadLayout
}

export interface GameSettings {
  /** null: the default in Settings. */
  gamemode: boolean | null
  mangohud: boolean | null
  close_steam_on_quit: boolean | null
  show_in_tether: boolean
  /** Epic on Linux: the Proton picked in the install sheet; null = Heroic's choice. */
  proton?: string | null
  /** Epic games: start with Epic's in-game overlay (store checkouts show in it). */
  eos_overlay?: boolean
  /** Linux, Proton games: this game's own controller mode; null = the default. */
  pad_in_games?: PadInGames | null
}

export interface LibraryStats {
  this_week_min: number
  last_14_days_min: number
  all_time_min: number
  most_played: GameId | null
}

/** The "Add any game" form. */
export interface ManualDraft {
  name: string
  /** A file path, or a command name when kind is "command". */
  path: string
  /** "auto" | "windows" | "appimage" | "native" | "command" */
  kind: string
  /** Extra arguments, typed as on a command line. */
  args: string
}

/** What the file picker (or a typed path) turned out to be. */
export interface Detected {
  name: string
  path: string
  kind: 'windows' | 'appimage' | 'native' | 'command'
  label: string
}

/** tether_status / tether-changed (src-tauri/src/tether_svc.rs `Snapshot`). Never contains a token. */
export interface TetherUserDto {
  id: string
  name: string
  initial: string
  colour: string
  /** grapple://avatar/<id>?v=<hash>, set only once the picture is cached; else draw the initial. */
  avatar: string | null
}

export interface TetherFriendDto extends TetherUserDto {
  online: boolean
  status: 'online' | 'idle' | 'dnd' | 'offline'
  playing: { game: string; name: string; since: number; owned: boolean } | null
}

export interface TetherSnapshot {
  /** The mock backend: the UI shows its fixture friends. */
  mock?: boolean
  loaded: boolean
  signed_in: boolean
  signing_in: boolean
  connection: 'off' | 'connecting' | 'online' | 'reconnecting' | 'error'
  error: string | null
  instance_url: string
  me: TetherUserDto | null
  friends: TetherFriendDto[]
}

// ---- the Settings page (src-tauri/src/backend/mod.rs DTOs)

export interface PlatformInfo {
  os: 'linux' | 'windows' | 'macos'
  version: string
}

export interface LibraryInfo {
  sources: { store: Store; games: number; installed: number }[]
  folders: { path: string; store: 'steam' | 'epic'; installed: number; free_bytes: number | null }[]
  /** Unix seconds. */
  last_scan: number | null
  hidden: { id: GameId; name: string }[]
}

export interface ProtonsInfo {
  /** "GE-Proton (latest)" first when there is one, then every version. */
  choices: string[]
  versions: { name: string; source: 'heroic' | 'steam-tools' | 'steam' }[]
  heroic_default: string | null
}

export interface AccountsInfo {
  steam_found: boolean
  /** Steam profile name, never the login name. */
  steam_name: string | null
  /** The store window has cookies to clear. */
  store_data: boolean
  epic_signed_in: boolean
  epic_name: string | null
}

export interface GameFiles {
  folder: string | null
  size_bytes: number | null
}

export interface StorageInfo {
  cache_path: string
  cache_bytes: number
  data_path: string
  data_bytes: number
  logs_path: string
  has_log: boolean
}

// ---- game invites (src-tauri/src/invites.rs; docs/tether-contract.md section 7)

export type InviteState = 'sent' | 'seen' | 'accepted' | 'declined' | 'cancelled' | 'expired'
/** What Play on an invite does on this computer. */
export type InvitePlayKind = 'launch' | 'install' | 'store' | 'missing'

/** One invite (`invites-changed`). Never holds a join payload. */
export interface InviteDto {
  id: string
  /** Josh sent it (else it was sent to him). */
  outgoing: boolean
  /** The other person. */
  friend: TetherUserDto
  game: { id: string; name: string }
  /** On grapple's clock: an open invite past its time is already 'expired'. */
  state: InviteState
  /** Milliseconds since the Unix epoch. */
  created_at: number
  expires_at: number
  answered_at: number | null
  /** Invites to Josh: what Play does here. */
  play: InvitePlayKind | null
  /** The library game it matched (for its art). */
  library_game: GameId | null
}

export interface InvitesSnapshot {
  /** off: not signed in · unsupported: this tether has no invites · needs_consent: "Allow invites" · ready */
  status: 'off' | 'unsupported' | 'needs_consent' | 'ready'
  /** Newest first, sent and received, the last day. */
  invites: InviteDto[]
  /** The invite a grapple://invite/<id> link opened: invite null while it loads (or before the sign-in). */
  open: { id: string; invite: InviteDto | null; error: string | null } | null
}

export type PlayAction =
  | { kind: 'launch'; game: GameId }
  | { kind: 'install'; game: GameId }
  | { kind: 'store'; key: string | null; query: string }
  | { kind: 'missing'; name: string }

/** invite_accept's answer and the `invite-play` event (Play pressed in tether). */
export interface PlayOutcome {
  action: PlayAction
  name: string
  /** Text that came with the invite (a private match), for the clipboard. */
  join: string | null
  error: string | null
}

export interface SendResult {
  friend: string
  ok: boolean
  error: string | null
}
