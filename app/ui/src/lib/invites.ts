// Game invites as the UI sees them (mockups/09-invites.html): the list Rust keeps (`invites_snapshot`, then
// `invites-changed`), the friend picker's state, and what Play on an invite opens here. Rust talks to tether and
// decides what Play means (src-tauri/src/invites.rs); an installed game is already launching when an answer comes.
import { copyText } from './clipboard'
import { gameById } from './games'
import { api, events } from './ipc'
import { installGame } from './library'
import { createStore, useStore } from './store'
import { openStoreGame, storeHome, storeSearch } from './store-nav'
import { toast } from './ui'
import type { GameId, InviteDto, InviteState, InvitesSnapshot, PlayOutcome } from './types'

interface InvitesState extends InvitesSnapshot {
  /** The friend picker (screen A), next to what opened it: the Home hero's Invite button (with the arrow), or the
   *  pointer (a game's menu). Page pixels (the interface size's zoom taken out). */
  picker: { game: GameId; at: PickerAnchor; arrow: boolean } | null
  /** "<name> joined" (screen D's toast). */
  joined: { name: string; initial: string; colour: string; avatar: string | null; at: number } | null
  /** Ticks every 30 s so "No reply · 9 min" and expiry stay true. */
  now: number
}

export const invitesStore = createStore<InvitesState>({ status: 'off', invites: [], open: null, picker: null, joined: null, now: Date.now() })
export const useInvites = <S>(select: (s: InvitesState) => S) => useStore(invitesStore, select)

/** An open invite past its time is expired (Rust says so too; this keeps it true between snapshots). */
export function stateAt(i: Pick<InviteDto, 'state' | 'expires_at'>, now: number): InviteState {
  return (i.state === 'sent' || i.state === 'seen') && i.expires_at <= now ? 'expired' : i.state
}
export const isOpen = (i: InviteDto, now: number) => { const s = stateAt(i, now); return s === 'sent' || s === 'seen' }

/** The sender's chip (screen D) and its tone. */
export function chip(i: InviteDto, now: number): { text: string; tone: '' | 'ok' | 'dim' } {
  switch (stateAt(i, now)) {
    case 'accepted': return { text: 'Joined', tone: 'ok' }
    case 'seen': return { text: 'Seen', tone: '' }
    case 'declined': return { text: 'Declined', tone: 'dim' }
    case 'cancelled': return { text: 'Cancelled', tone: 'dim' }
    case 'expired': return { text: 'Expired', tone: 'dim' }
    default: {
      const min = Math.floor((now - i.created_at) / 60_000)
      return min < 1 ? { text: 'Sent', tone: '' } : { text: `No reply · ${min} min`, tone: 'dim' }
    }
  }
}

/** Play's word for an invite to Josh: what it does here. */
export const playWord = (i: Pick<InviteDto, 'play'>) => (i.play === 'install' ? 'Install' : i.play === 'store' ? 'Get' : 'Play')

/** How long a sent invite stays in the list after it ended (so "Lerato joined" can still be seen). */
const KEEP_ENDED_MS = 15 * 60_000

/** What the top bar's Invites shows: sent invites grouped by game (newest group first; a group stays while one of
 *  its invites is open or ended less than 15 minutes ago) and the open invites to Josh. */
export function visible(list: InviteDto[], now: number) {
  const groups: { game: InviteDto['game']; invites: InviteDto[] }[] = []
  for (const i of list) {
    if (!i.outgoing) continue
    const g = groups.find((x) => x.game.id === i.game.id)
    if (g) g.invites.push(i)
    else groups.push({ game: i.game, invites: [i] })
  }
  const recent = (i: InviteDto) => isOpen(i, now) || now - (i.answered_at ?? Math.min(i.expires_at, now)) < KEEP_ENDED_MS
  const sent = groups
    .filter((g) => g.invites.some(recent))
    // The newest invite per friend only (an "Invite again" replaces the old row).
    .map((g) => ({ ...g, invites: g.invites.filter((i, n, all) => all.findIndex((x) => x.friend.id === i.friend.id) === n) }))
  const received = list.filter((i) => !i.outgoing && isOpen(i, now))
  return { sent, received, count: sent.reduce((n, g) => n + g.invites.length, 0) + received.length }
}

// ---------------------------------------------------------------------------------------------- following Rust

let known: Map<string, InviteState> | null = null
let joinedTimer = 0

/** Toasts for what changed: a friend joined (screen D), someone invited Josh. Nothing on the first snapshot. */
function noteChanges(next: InviteDto[]) {
  const before = known
  known = new Map(next.map((i) => [i.id, i.state]))
  if (!before) return
  for (const i of next) {
    const was = before.get(i.id)
    if (i.outgoing && i.state === 'accepted' && was !== 'accepted') {
      invitesStore.set({ joined: { ...i.friend, at: Date.now() } })
      clearTimeout(joinedTimer)
      joinedTimer = window.setTimeout(() => invitesStore.set({ joined: null }), 4000)
    } else if (!i.outgoing && was === undefined && isOpen(i, Date.now())) {
      toast(`${i.friend.name} invited you to ${i.game.name}`)
    }
  }
}

function apply(s: InvitesSnapshot) {
  noteChanges(s.invites)
  invitesStore.set({ status: s.status, invites: s.invites, open: s.open, now: Date.now() })
}

let started = false
export async function startInvites() {
  if (started) return
  started = true
  void events.invitesChanged(apply)
  void events.invitePlay((o) => handlePlay(o))
  window.setInterval(() => invitesStore.set({ now: Date.now() }), 30_000)
  try {
    apply(await api.invitesSnapshot())
  } catch {
    /* no backend answer: the Invite buttons just stay away */
  }
}

// ---------------------------------------------------------------------------------------------- actions

export interface PickerAnchor { left: number; top: number; right: number; bottom: number }
export const openPicker = (game: GameId, at: PickerAnchor, arrow = false) => invitesStore.set({ picker: { game, at, arrow } })
export const closePicker = () => invitesStore.set({ picker: null })

const names = (list: string[]) => (list.length > 1 ? `${list.slice(0, -1).join(', ')} and ${list[list.length - 1]}` : list[0])

/** Send one invite per friend. Resolves true when every one went out. */
export async function sendInvites(game: GameId, friends: { id: string; name: string }[]): Promise<boolean> {
  try {
    const results = await api.inviteSend(game, friends.map((f) => f.id))
    const ok = friends.filter((f) => results.find((r) => r.friend === f.id)?.ok)
    const failed = results.find((r) => !r.ok)
    if (failed) toast(failed.error ?? 'Not sent')
    else if (ok.length) toast(`Invited ${names(ok.map((f) => f.name))}`)
    return !failed
  } catch (e) {
    toast(String(e))
    return false
  }
}

/** "Invite again": the friends of this group whose invite ended without them joining. */
export function againList(invites: InviteDto[], now: number): InviteDto[] {
  return invites.filter((i) => ['declined', 'expired', 'cancelled'].includes(stateAt(i, now)))
}

export async function inviteAgain(invites: InviteDto[]) {
  const now = Date.now()
  const again = againList(invites, now)
  if (again.length) await sendInvites(again[0].game.id, again.map((i) => i.friend))
}

/** "Cancel invite": every invite of this group that is still open. */
export async function cancelOpen(invites: InviteDto[]) {
  const now = Date.now()
  for (const i of invites.filter((x) => isOpen(x, now))) {
    try {
      await api.inviteCancel(i.id)
    } catch (e) {
      toast(String(e))
      return
    }
  }
}

export async function decline(i: InviteDto) {
  try {
    await api.inviteDecline(i.id)
  } catch (e) {
    toast(String(e))
  }
}

/** Invites to Josh shown in the Invites list: tell the sender they were seen. */
export function markSeen(list: InviteDto[]) {
  const ids = list.filter((i) => !i.outgoing && i.state === 'sent').map((i) => i.id)
  if (ids.length) api.invitesSeen(ids).catch(() => {})
}

/** Play on an invite to Josh (pressed by the person): accepted by Rust, which launches an installed game. */
export async function accept(i: Pick<InviteDto, 'id'>) {
  try {
    handlePlay(await api.inviteAccept(i.id))
  } catch (e) {
    toast(String(e))
  }
}

/** What Rust did or wants opened after Play (here, or in tether: `invite-play`). */
export function handlePlay(o: PlayOutcome) {
  const a = o.action
  if (o.error) toast(o.error)
  else if (a.kind === 'install') {
    const g = gameById(a.game)
    if (g) installGame(g)
    else toast(`${o.name} isn't in your library`)
  } else if (a.kind === 'store') {
    if (a.key) openStoreGame(a.key)
    else {
      storeSearch.set({ q: a.query })
      storeHome()
    }
  } else if (a.kind === 'missing') {
    toast(`${o.name} isn't on this computer`)
  }
  if (o.join) {
    const join = o.join
    void copyText(join).then((ok) => toast(ok ? 'Match details copied' : "Couldn't copy the match details"))
  }
}

export const closeOpen = () => { void api.inviteClose() }
