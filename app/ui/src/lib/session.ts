// Launch / playing state, driven by the backend's events. The UI never decides a game is running.
import { api, events } from './ipc'
import { createStore, useStore } from './store'
import { refreshGames } from './games'
import { settingsStore } from './settings'
import type { GameId, LaunchEvent } from './types'

export interface Step {
  label: string
  done: boolean
}

interface SessionState {
  phase: 'idle' | 'launching' | 'playing' | 'failed'
  game: GameId | null
  steps: Step[]
  startedAt: number | null
  message: string | null
  /** True when this window shrank to the playing card for the session (we started the game). */
  compact: boolean
  /** True when grapple hid itself for the session ("When a game starts: Hide grapple"). */
  hid: boolean
}

const idle: SessionState = { phase: 'idle', game: null, steps: [], startedAt: null, message: null, compact: false, hid: false }
export const sessionStore = createStore<SessionState>(idle)
export const useSession = <S>(select: (s: SessionState) => S) => useStore(sessionStore, select)

export function handleLaunchEvent(e: LaunchEvent) {
  const s = sessionStore.get()
  switch (e.kind) {
    case 'step': {
      const steps = s.steps.some((x) => x.label === e.label)
        ? s.steps.map((x) => (x.label === e.label ? { ...x, done: e.done } : x))
        : [...s.steps, { label: e.label, done: e.done }]
      sessionStore.set({ steps, phase: s.phase === 'idle' ? 'launching' : s.phase })
      break
    }
    case 'running': {
      // Only a game grapple started changes the window (Settings › General › When a game starts).
      const mode = s.phase === 'launching' ? settingsStore.get().on_game_start : 'keep'
      const compact = mode === 'card'
      const hid = mode === 'hide'
      sessionStore.set({ phase: 'playing', game: e.game, startedAt: e.started_at, compact, hid, steps: [] })
      if (compact) void api.setCompact(true)
      if (hid) void api.hideMain()
      break
    }
    case 'exited': {
      // When the game closes: bring grapple back, or stay hidden.
      const back = settingsStore.get().on_game_exit !== 'stay'
      if (s.compact) {
        void api.setCompact(false).then(() => { if (!back) void api.hideMain() })
      }
      if (s.hid && back) void api.showMain()
      sessionStore.set(idle)
      void refreshGames()
      break
    }
    case 'failed':
      if (s.compact) void api.setCompact(false)
      if (s.hid) void api.showMain() // the failure needs to be seen
      sessionStore.set({ ...idle, phase: 'failed', game: e.game, message: e.message })
      break
  }
}

export function dismissFailure() {
  sessionStore.set(idle)
}

export function play(id: GameId) {
  const s = sessionStore.get()
  if (s.phase === 'launching' || s.phase === 'playing') return
  api.launch(id).catch((e) => sessionStore.set({ ...idle, phase: 'failed', game: id, message: String(e) }))
}

export function stop() {
  const g = sessionStore.get().game
  if (g) void api.stop(g)
}

/** Back to the full window while the game keeps running. */
export function expand() {
  if (sessionStore.get().compact) {
    sessionStore.set({ compact: false })
    void api.setCompact(false)
  }
}

let started = false
export function startSession() {
  if (started) return
  started = true
  void events.launchStarted((e) => sessionStore.set({ ...idle, phase: 'launching', game: e.game }))
  void events.launchEvent(handleLaunchEvent)
  // A game may already be running (Grapple was restarted, or the watcher saw it).
  void api.running().then((r) => {
    const first = r[0]
    if (first && sessionStore.get().phase === 'idle') {
      sessionStore.set({ phase: 'playing', game: first[0], startedAt: first[1], compact: false, hid: false })
    }
  })
}
