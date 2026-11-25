// tether (friends, sign-in) as the UI sees it.
//
// The Rust side owns tether: tokens, the browser sign-in, the gateway connection. This store only mirrors the
// snapshot it sends (`tether_status` once, then the `tether-changed` event) and forwards the button clicks.
// Nothing here ever sees a token.
//
// Fixture friends (./mock.ts) are used only when there is no real backend to ask: `vite dev` in a plain
// browser, or the Rust `mock` feature (its tether_status answers `mock: true`).
import { api, events, inTauri } from '../lib/ipc'
import { createStore, useStore } from '../lib/store'
import type { GameId, TetherSnapshot } from '../lib/types'

export interface Friend {
  id: string
  name: string
  /** One letter for the avatar. */
  initial: string
  /** CSS token for the avatar background, e.g. "var(--avatar-1)". */
  colour: string
  /** The profile picture's URL once it is cached, else null (draw the initial). */
  avatar: string | null
  online: boolean
  /** online | idle | dnd | offline (the invite picker's line). */
  status: TetherSnapshot['friends'][number]['status']
  /** The game they are in now, with the unix start time. `game` is the library game's id when you own it
   *  (so covers can show them), else a `tether:...` key that groups friends in the same game. */
  playing: { game: GameId; name: string; since: number } | null
}

export interface TetherState {
  /** The first check (saved sign-in, keyring) has finished: until then the sign-in card stays away. */
  loaded: boolean
  signedIn: boolean
  /** The first-run card was dismissed with "Skip for now". */
  skipped: boolean
  /** Waiting for the person in the browser. */
  signingIn: boolean
  connection: TetherSnapshot['connection']
  error: string | null
  instanceUrl: string
  me: { name: string; initial: string; colour: string; avatar: string | null }
  friends: Friend[]
  /** Per-game one-liners ("rowan is playing this now"), only from what tether really reported. */
  notes: Record<GameId, string>
}

const SKIP_KEY = 'grapple.tether.skipped'
const SIGNED_KEY = 'grapple.tether.signedin' // mock mode only
function flag(k: string) {
  try {
    return localStorage.getItem(k) === '1'
  } catch {
    return false
  }
}
function setFlag(k: string, v: boolean) {
  try {
    localStorage.setItem(k, v ? '1' : '0')
  } catch {
    /* private mode: the card just shows again next start */
  }
}

const NOBODY = { name: '', initial: '?', colour: 'var(--avatar-offline)', avatar: null }

// Dev-only (stripped from release builds): ?signedin skips the first-run card, for screenshots.
const devSignedIn = import.meta.env.DEV && location.search.includes('signedin')

export const tetherStore = createStore<TetherState>({
  loaded: !inTauri, // a plain browser has no backend to wait for
  signedIn: !inTauri && (devSignedIn || flag(SIGNED_KEY)),
  skipped: flag(SKIP_KEY),
  signingIn: false,
  connection: 'off',
  error: null,
  instanceUrl: '',
  me: NOBODY,
  friends: [],
  notes: {},
})

export const useTether = <S>(select: (s: TetherState) => S) => useStore(tetherStore, select)

const joinNames = (names: string[]) => (names.length > 1 ? `${names.slice(0, -1).join(', ')} and ${names[names.length - 1]}` : names[0])

/** "rowan is playing this now" per game, from who is really in it. Fake hour counts are gone: tether does not
 *  report anyone's playtime. */
export function deriveNotes(friends: Friend[]): Record<GameId, string> {
  const by = new Map<GameId, string[]>()
  for (const f of friends) if (f.playing) by.set(f.playing.game, [...(by.get(f.playing.game) ?? []), f.name])
  const notes: Record<GameId, string> = {}
  for (const [game, names] of by) notes[game] = `${joinNames(names)} ${names.length > 1 ? 'are' : 'is'} playing now`
  return notes
}

function fromSnapshot(s: TetherSnapshot): Partial<TetherState> {
  const friends: Friend[] = s.friends.map((f) => ({
    id: f.id,
    name: f.name,
    initial: f.initial,
    colour: f.colour,
    avatar: f.avatar ?? null,
    online: f.online,
    status: f.status,
    playing: f.playing ? { game: f.playing.game, name: f.playing.name, since: f.playing.since } : null,
  }))
  return {
    loaded: s.loaded,
    signedIn: s.signed_in,
    signingIn: s.signing_in,
    connection: s.connection,
    error: s.error,
    instanceUrl: s.instance_url,
    me: s.me ? { name: s.me.name, initial: s.me.initial, colour: s.me.colour, avatar: s.me.avatar ?? null } : NOBODY,
    friends,
    notes: deriveNotes(friends),
  }
}

let mock = !inTauri

async function loadMock() {
  mock = true
  const { MOCK_FRIENDS, MOCK_ME } = await import('./mock')
  tetherStore.set({ loaded: true, me: MOCK_ME, friends: MOCK_FRIENDS, notes: deriveNotes(MOCK_FRIENDS), signedIn: flag(SIGNED_KEY) || devSignedIn, connection: 'online', instanceUrl: '' })
}

function apply(s: TetherSnapshot) {
  if (s.mock) void loadMock()
  else tetherStore.set(fromSnapshot(s))
}

let started = false
/** Ask the backend once and follow `tether-changed`. Call from the main window only. */
export async function startTether() {
  if (started) return
  started = true
  if (!inTauri) return void loadMock()
  void events.tetherChanged(apply)
  try {
    apply(await api.tetherStatus())
  } catch (e) {
    tetherStore.set({ loaded: true, error: String(e) })
  }
}

export const tether = {
  /** Opens the system browser for the sign-in and waits. Only ever called from a button the person clicked. */
  async signIn() {
    if (mock) {
      setFlag(SIGNED_KEY, true)
      tetherStore.set({ signedIn: true })
      return
    }
    tetherStore.set({ signingIn: true, error: null })
    try {
      apply(await api.tetherSignIn())
    } catch (e) {
      const msg = String(e)
      tetherStore.set({ signingIn: false, error: /cancelled/i.test(msg) ? null : msg })
    }
  },
  cancelSignIn() {
    if (!mock) void api.tetherCancelSignIn()
  },
  async signOut() {
    if (mock) {
      setFlag(SIGNED_KEY, false)
      tetherStore.set({ signedIn: false })
      return
    }
    try {
      apply(await api.tetherSignOut())
    } catch (e) {
      tetherStore.set({ error: String(e) })
    }
  },
  /** Point Grapple at another tether (the `tether.instance_url` setting). Resolves with an error text or null. */
  async setInstanceUrl(url: string): Promise<string | null> {
    if (mock) return null
    try {
      apply(await api.tetherSetInstanceUrl(url.trim()))
      return null
    } catch (e) {
      return String(e)
    }
  },
  skip() {
    setFlag(SKIP_KEY, true)
    tetherStore.set({ skipped: true })
  },
  /** Friends currently in this game. */
  friendsIn(s: TetherState, game: GameId): Friend[] {
    return s.friends.filter((f) => f.playing?.game === game)
  },
  /** Friends who are in any game, grouped by game, biggest group first. */
  playingGroups(s: TetherState): { game: GameId; name: string; friends: Friend[]; since: number }[] {
    const m = new Map<GameId, { game: GameId; name: string; friends: Friend[]; since: number }>()
    for (const f of s.friends) {
      if (!f.playing) continue
      const g = m.get(f.playing.game) ?? { game: f.playing.game, name: f.playing.name, friends: [], since: f.playing.since }
      g.friends.push(f)
      g.since = Math.min(g.since, f.playing.since)
      m.set(f.playing.game, g)
    }
    return [...m.values()].sort((a, b) => b.friends.length - a.friends.length)
  },
}
