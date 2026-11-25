import { api, events } from './ipc'
import { createStore, useStore } from './store'
import type { Game, GameId, LibraryStats, PlayStats } from './types'

interface GamesState {
  /** What the lists show: every game the settings do not hide. */
  games: Game[]
  /** Every game, hidden ones too (a hidden game can still run, and its settings sheet stays open). */
  all: Game[]
  loaded: boolean
  /** Home hero / drawer selection. */
  selected: GameId | null
  stats: Record<GameId, PlayStats>
  library: LibraryStats | null
}

export const gamesStore = createStore<GamesState>({ games: [], all: [], loaded: false, selected: null, stats: {}, library: null })
export const useGames = <S>(select: (s: GamesState) => S) => useStore(gamesStore, select)

/** Played, most recent first, then never-played by name: the order of the shelf and the library wall. */
export function byRecent(a: Game, b: Game): number {
  return (b.last_played ?? 0) - (a.last_played ?? 0) || a.sort_name.localeCompare(b.sort_name)
}

export async function refreshGames() {
  const all = await api.listGames()
  const games = all.filter((g) => !g.hidden)
  const cur = gamesStore.get().selected
  const installed = games.filter((g) => g.installed).sort(byRecent)
  gamesStore.set({
    games,
    all,
    loaded: true,
    selected: cur && games.some((g) => g.id === cur) ? cur : (installed[0]?.id ?? games[0]?.id ?? null),
  })
  // Stats of the selected game may have changed (new session).
  const s = gamesStore.get().selected
  if (s) void loadStats(s, true)
  void api.libraryStats().then((library) => gamesStore.set({ library }))
}

export async function loadStats(id: GameId, force = false) {
  if (!force && gamesStore.get().stats[id]) return
  const st = await api.gameStats(id)
  gamesStore.set((s) => ({ stats: { ...s.stats, [id]: st } }))
}

export function select(id: GameId) {
  gamesStore.set({ selected: id })
  void loadStats(id)
}

export function gameById(id: GameId | null | undefined): Game | undefined {
  return id ? gamesStore.get().all.find((g) => g.id === id) : undefined
}

export async function toggleFavourite(id: GameId) {
  const g = gameById(id)
  if (!g) return
  const on = !g.favourite
  const flip = (x: Game) => (x.id === id ? { ...x, favourite: on } : x)
  gamesStore.set((s) => ({ games: s.games.map(flip), all: s.all.map(flip) }))
  await api.setFavourite(id, on)
}

let started = false
export function startGames() {
  if (started) return
  started = true
  void refreshGames()
  void events.gamesChanged(() => void refreshGames())
}
