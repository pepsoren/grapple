// Mirrors crates/grapple-download/src/types.rs (docs/DOWNLOADS_PLAN.md "Tauri commands, events and types").
import type { GameId } from './types'

export type DlStore = 'steam' | 'epic'
export type DlState = 'queued' | 'starting' | 'downloading' | 'verifying' | 'installing' | 'paused' | 'done' | 'error'

export interface DlItem {
  game: GameId
  store: DlStore
  kind: 'install' | 'update'
  state: DlState
  name: string
  /** grapple://art/... URLs like Game art. */
  art: { hero?: string; header?: string; cover?: string }
  bytesDone: number
  bytesTotal: number | null
  bytesPerSec: number | null
  secondsLeft: number | null
  /** Last 20 samples, bytes/s, for the sparkline. */
  speedHistory: number[]
  targetDir: string
  position: number
  canPause: boolean
  canReorder: boolean
  canCancel: boolean
  error?: string
  finishedAt?: number
}

export interface DlSnapshot {
  active?: DlItem
  queued: DlItem[]
  updates: DlItem[]
  done: DlItem[]
  pausedAll: boolean
  steamBridge: 'off' | 'on' | 'unavailable' | 'restart-pending'
}

export interface InstallTarget {
  /** "~/…" under the home folder; install_start takes it back. */
  path: string
  label: string
  freeBytes: number
  totalBytes: number
  enough: boolean
  isDefault: boolean
}

export interface InstallPlan {
  game: GameId
  store: DlStore
  sizeBytes: number | null
  targets: InstallTarget[]
  /** Epic on Linux only. */
  protons?: { name: string; isDefault: boolean }[]
}

export interface DlSettings {
  /** 0 = none */
  speedLimitBytes: number
  connections: number
  autoUpdate: boolean
  updateOnlyWhenIdle: boolean
  pauseWhilePlaying: boolean
  notifyReady: boolean
  steamDefaultDir?: string
  epicDefaultDir: string
  steamBridge: boolean
  steamBridgePort?: number
  /** Read-only: the folders the pills offer. */
  choices?: { steam: string[]; epic: string[] }
}
