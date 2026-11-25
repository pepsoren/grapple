// Browser-only stand-in for game invites (vite dev in a plain browser, screenshots), like download-mock.ts. Mirrors
// the Rust mock (src-tauri/src/backend/mock.rs): sent invites are seen after 2 s, the first friend joins at 4 s and
// the second declines at 6 s; a friend invites Josh a little after the start. The friends are tether/mock.ts's.
// Dev-only URL flags for screenshots: ?invites (a group like mockup D, and an invite to Josh), ?invite=<id> (the
// view a grapple://invite/<id> link opens).
import type { GameId, InviteDto, InvitesSnapshot, PlayAction, PlayOutcome, SendResult } from './types'

type Emit = (event: string, p: unknown) => void
interface G { id: GameId; name: string; installed: boolean; store: string }

const FRIENDS: Record<string, [string, number]> = { rowan: ['rowan', 1], kai: ['kai', 4], mira: ['mira', 2], sipho: ['Sipho', 5], dev: ['Dev', 3], thabo: ['Thabo', 6] }
const person = (id: string) => {
  const [name, slot] = FRIENDS[id] ?? ['Someone', 0]
  return { id, name, initial: name[0].toUpperCase(), colour: `var(--avatar-${slot})`, avatar: null }
}

let emit: Emit = () => {}
let games: () => G[] = () => []
let list: InviteDto[] = []
let open: InvitesSnapshot['open'] = null
let from: 'friends' | 'nobody' = 'friends'
// Snowflakes are past JavaScript's safe integers: count in the last digits only.
let next = 0
let started = false

function decide(id: GameId, name: string): PlayAction {
  const g = games().find((x) => x.id === id)
  if (g?.installed) return { kind: 'launch', game: g.id }
  if (g && g.store !== 'manual') return { kind: 'install', game: g.id }
  if (id.startsWith('steam:')) return { kind: 'store', key: id, query: name }
  if (id.startsWith('epic:')) return { kind: 'store', key: null, query: name }
  return { kind: 'missing', name }
}

function make(friend: string, outgoing: boolean, game: GameId, name: string, ageMin = 0): InviteDto {
  const a = decide(game, name)
  const at = Date.now() - ageMin * 60_000
  return {
    id: `15751623975119${String(++next).padStart(5, '0')}`, outgoing, friend: person(friend), game: { id: game, name }, state: 'sent', created_at: at, expires_at: at + 15 * 60_000,
    answered_at: null, play: outgoing ? null : a.kind, library_game: a.kind === 'launch' || a.kind === 'install' ? a.game : null,
  }
}

const snapshot = (): InvitesSnapshot => ({ status: 'ready', invites: list, open })
const changed = () => { list = [...list]; emit('invites-changed', snapshot()) }
function set(id: string, state: InviteDto['state']) {
  list = list.map((i) => (i.id === id ? { ...i, state, answered_at: state === 'seen' ? i.answered_at : Date.now() } : i))
  changed()
}

export function startInviteMock(e: Emit, g: () => G[]) {
  emit = e
  games = g
  if (started) return
  started = true
  const q = new URLSearchParams(location.search)
  if (q.has('invites')) {
    const rl = ['epic:sugar', 'Rocket League'] as const
    const group = [make('rowan', true, ...rl, 3), make('kai', true, ...rl, 3), make('mira', true, ...rl, 9), make('sipho', true, ...rl, 9)]
    group[0].state = 'accepted'
    group[1].state = 'seen'
    group[2].state = 'declined'
    list = [make('dev', false, 'steam:291550', 'Brawlhalla', 2), ...group]
  } else {
    setTimeout(() => { list = [make('mira', false, 'steam:291550', 'Brawlhalla'), ...list]; changed() }, 12_000)
  }
  const link = q.get('invite')
  if (link) open = { id: link, invite: { ...make('rowan', false, 'steam:730', 'Counter-Strike 2'), id: link }, error: null }
}

export async function invitesInvoke(cmd: string, args: Record<string, unknown>, launch: (id: GameId) => void): Promise<unknown> {
  switch (cmd) {
    case 'invites_snapshot': return snapshot()
    case 'invite_send': {
      const game = games().find((g) => g.id === args.game)
      if (!game) throw 'unknown game'
      const friends = args.friends as string[]
      const sent = friends.map((f) => make(f, true, game.id, game.name))
      list = [...sent.reverse(), ...list]
      changed()
      const ids = [...sent].reverse().map((i) => i.id)
      setTimeout(() => ids.forEach((id) => { if (list.find((i) => i.id === id)?.state === 'sent') set(id, 'seen') }), 2000)
      ;(['accepted', 'declined'] as const).forEach((st, n) => setTimeout(() => {
        if (ids[n] && list.find((i) => i.id === ids[n])?.state === 'seen') set(ids[n], st)
      }, 4000 + n * 2000))
      return friends.map((friend): SendResult => ({ friend, ok: true, error: null }))
    }
    case 'invite_cancel': set(String(args.id), 'cancelled'); return
    case 'invite_decline': set(String(args.id), 'declined'); return
    case 'invites_seen': (args.ids as string[]).forEach((id) => { if (list.find((i) => i.id === id)?.state === 'sent') set(id, 'seen') }); return
    case 'invite_accept': {
      const id = String(args.id)
      if (open?.id === id && open.invite) { list = [open.invite, ...list.filter((i) => i.id !== id)]; open = null }
      const i = list.find((x) => x.id === id)
      if (!i) throw "This invite isn't for this account"
      set(id, 'accepted')
      const action = decide(i.game.id, i.game.name)
      if (action.kind === 'launch') launch(action.game)
      return { action, name: i.game.name, join: null, error: null } satisfies PlayOutcome
    }
    case 'invite_close': open = null; changed(); return
    case 'invite_settings_get': return from
    case 'invite_settings_set': from = args.from as typeof from; return from
    default: return
  }
}
