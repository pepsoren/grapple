// MOCK data for the tether store (see ./index.ts), used only without a real backend (browser dev, Rust `mock`
// feature). Mirrors the friends of mockups/04-collection.html. Hour counts are not mocked: tether does not report them.
import type { Friend } from './index'

const t = () => Math.floor(Date.now() / 1000)

export const MOCK_ME = { name: 'josh', initial: 'J', colour: 'var(--avatar-0)', avatar: null }

export const MOCK_FRIENDS: Friend[] = [
  { id: 'rowan', name: 'rowan', initial: 'R', colour: 'var(--avatar-1)', avatar: null, online: true, status: 'online',
    playing: { game: 'steam:730', name: 'Counter-Strike 2', since: t() - 38 * 60 } },
  { id: 'kai', name: 'kai', initial: 'K', colour: 'var(--avatar-4)', avatar: null, online: true, status: 'online',
    playing: { game: 'steam:730', name: 'Counter-Strike 2', since: t() - 38 * 60 } },
  { id: 'mira', name: 'mira', initial: 'M', colour: 'var(--avatar-2)', avatar: null, online: true, status: 'online',
    playing: { game: 'steam:291550', name: 'Brawlhalla', since: t() - 12 * 60 } },
  // Not playing: only the invite picker shows them.
  { id: 'sipho', name: 'Sipho', initial: 'S', colour: 'var(--avatar-5)', avatar: null, online: true, status: 'online', playing: null },
  { id: 'dev', name: 'Dev', initial: 'D', colour: 'var(--avatar-3)', avatar: null, online: true, status: 'idle', playing: null },
  { id: 'thabo', name: 'Thabo', initial: 'T', colour: 'var(--avatar-6)', avatar: null, online: false, status: 'offline', playing: null },
]
