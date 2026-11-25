// Game invites in grapple (mockups/09-invites.html): A · the Invite button and the friend picker, D · the top bar's
// Invites with each friend's state (and invites to Josh, with Play / Decline), the "<name> joined" toast, and the
// invite a grapple://invite/<id> link opened (one Play button, pressed by the person; a link never plays by itself).
// v1 sends direct messages only, so the mockup's "Direct message / Voice" choice is not drawn.
import { useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react'
import { createPortal } from 'react-dom'
import { gameById, useGames } from '../lib/games'
import {
  accept, againList, cancelOpen, chip, closeOpen, closePicker, decline, inviteAgain, isOpen, markSeen, openPicker, playWord, sendInvites,
  stateAt, useInvites, visible, type PickerAnchor,
} from '../lib/invites'
import { uiZoom } from '../lib/theme'
import type { GameId, InviteDto } from '../lib/types'
import { tether, useTether, type Friend } from '../tether'
import { Avatar } from './Avatar'
import { backdrop } from './Art'
import { CheckIcon, CloseIcon, InviteIcon, SearchIcon } from './Icons'
import '../styles/invites.css'

/** Signed in to tether, and this tether has invites (with or without the sign-in's OK yet). */
export function useCanInvite(): boolean {
  const signedIn = useTether((s) => s.signedIn)
  const status = useInvites((s) => s.status)
  return signedIn && (status === 'ready' || status === 'needs_consent')
}

/** Page pixels of an element (the interface size is a zoom on the page). */
function anchorOf(el: Element): PickerAnchor {
  const r = el.getBoundingClientRect()
  const z = uiZoom()
  return { left: r.left / z, top: r.top / z, right: r.right / z, bottom: r.bottom / z }
}

/** Screen A: the ghost Invite button beside Play. Hidden when invites can't be sent from here. */
export function InviteButton({ game }: { game: GameId }) {
  const can = useCanInvite()
  const open = useInvites((s) => s.picker?.game === game && s.picker.arrow)
  if (!can) return null
  return (
    <button
      className={`btn ghost inv-open${open ? ' on' : ''}`}
      aria-haspopup="dialog"
      aria-expanded={open}
      onClick={(e) => (open ? closePicker() : openPicker(game, anchorOf(e.currentTarget), true))}
    >
      <InviteIcon />Invite
    </button>
  )
}

/** "Invite…" from a game's menu: the picker at the pointer. */
export const invitePickerAt = (game: GameId, x: number, y: number) => {
  const z = uiZoom()
  openPicker(game, { left: x / z, top: y / z, right: x / z, bottom: y / z })
}

function statusLine(f: Friend, game: string | undefined): { text: string; live: boolean } {
  if (f.playing) return { text: `In ${f.playing.name}`, live: f.playing.game === game || f.playing.name === game }
  return { text: { online: 'Online', idle: 'Idle', dnd: 'Busy', offline: 'Offline' }[f.status] ?? 'Offline', live: false }
}

const RANK = { online: 1, dnd: 2, idle: 2, offline: 3 } as const

/** The friend picker (screen A): search, friends with their status, several at once, Send. */
export function InvitePicker() {
  const p = useInvites((s) => s.picker)
  const status = useInvites((s) => s.status)
  const friends = useTether((s) => s.friends)
  const signingIn = useTether((s) => s.signingIn)
  const game = useGames((s) => (p ? s.all.find((g) => g.id === p.game) : undefined))
  const [q, setQ] = useState('')
  const [picked, setPicked] = useState<string[]>([])
  const [sending, setSending] = useState(false)
  const box = useRef<HTMLDivElement>(null)
  const [pos, setPos] = useState<{ left: number; top: number; up: boolean; arrow: number } | null>(null)

  useEffect(() => { setQ(''); setPicked([]) }, [p?.game])
  // Above the anchor when there is room (as in the mockup), else under it; always inside the window.
  useLayoutEffect(() => {
    if (!p || !box.current) return setPos(null)
    const z = uiZoom()
    const w = box.current.offsetWidth
    const h = box.current.offsetHeight
    const vw = window.innerWidth / z
    const vh = window.innerHeight / z
    const gap = p.arrow ? 14 : 4
    const up = p.at.top - gap - h >= 76
    const left = Math.max(8, Math.min(p.arrow ? p.at.left - 14 : p.at.left, vw - w - 8))
    const top = up ? p.at.top - gap - h : Math.min(p.at.bottom + gap, vh - h - 8)
    setPos({ left, top, up, arrow: Math.max(18, Math.min((p.at.left + p.at.right) / 2 - left - 6, w - 30)) })
  }, [p, status, friends.length])
  useEffect(() => {
    if (!p) return
    const down = (e: PointerEvent) => {
      const t = e.target as Element
      if (!box.current?.contains(t) && !t.closest?.('.inv-open')) closePicker()
    }
    const key = (e: KeyboardEvent) => { if (e.key === 'Escape') { e.stopPropagation(); closePicker() } }
    document.addEventListener('pointerdown', down, true)
    document.addEventListener('keydown', key, true)
    window.addEventListener('resize', closePicker)
    return () => {
      document.removeEventListener('pointerdown', down, true)
      document.removeEventListener('keydown', key, true)
      window.removeEventListener('resize', closePicker)
    }
  }, [p])

  const list = useMemo(() => {
    const ql = q.trim().toLowerCase()
    const here = (f: Friend) => (f.playing && (f.playing.game === p?.game || f.playing.name === game?.name) ? 0 : 1)
    return friends
      .filter((f) => !ql || f.name.toLowerCase().includes(ql))
      .sort((a, b) => here(a) - here(b) || RANK[a.status] - RANK[b.status] || a.name.localeCompare(b.name))
  }, [friends, q, p?.game, game?.name])

  if (!p) return null
  const toggle = (id: string) => setPicked((cur) => (cur.includes(id) ? cur.filter((x) => x !== id) : [...cur, id]))
  const send = async () => {
    if (!picked.length || sending) return
    setSending(true)
    const who = friends.filter((f) => picked.includes(f.id)).map((f) => ({ id: f.id, name: f.name }))
    const ok = await sendInvites(p.game, who)
    setSending(false)
    if (ok) closePicker()
  }
  return createPortal(
    <div
      ref={box}
      className={`inv-pop inv-picker${p.arrow ? (pos?.up === false ? ' arrow-up' : ' arrow-down') : ''}`}
      role="dialog"
      aria-label={`Invite friends to ${game?.name ?? 'this game'}`}
      style={{ left: pos?.left ?? 0, top: pos?.top ?? 0, visibility: pos ? 'visible' : 'hidden', ['--arrow-x' as string]: `${pos?.arrow ?? 40}px` }}
    >
      {status === 'needs_consent' ? (
        <div className="inv-consent">
          <span className="sub">Invites need your OK in tether.</span>
          <button className="btn play sm" disabled={signingIn} onClick={() => void tether.signIn()}>{signingIn ? 'Waiting for the browser…' : 'Allow invites'}</button>
        </div>
      ) : (
        <>
          <label className="inv-find">
            <SearchIcon size={15} />
            <input
              autoFocus
              value={q}
              placeholder="Find a friend"
              spellCheck={false}
              onChange={(e) => setQ(e.target.value)}
              onKeyDown={(e) => { if (e.key !== 'Escape') e.stopPropagation() }}
            />
          </label>
          <div className="inv-fl">
            {list.map((f) => {
              const st = statusLine(f, game?.name)
              const on = picked.includes(f.id)
              return (
                <button key={f.id} className={`inv-fr${on ? ' on' : ''}`} role="checkbox" aria-checked={on} onClick={() => toggle(f.id)}>
                  <Avatar f={f} noTitle />
                  <span className="who"><b>{f.name}</b><span className={`sub${st.live ? ' live' : ''}`}>{st.text}</span></span>
                  <span className="chk">{on && <CheckIcon size={12} />}</span>
                </button>
              )
            })}
            {!list.length && <span className="sub inv-none">{friends.length ? 'No one by that name' : 'No friends on tether yet'}</span>}
          </div>
          <div className="inv-foot">
            <span className="sub">{picked.length} selected</span>
            <button className="btn play sm" disabled={!picked.length || sending} onClick={() => void send()}>Send</button>
          </div>
        </>
      )}
    </div>,
    document.body,
  )
}

function SentRow({ i, now }: { i: InviteDto; now: number }) {
  const c = chip(i, now)
  return (
    <div className="inv-sr">
      <Avatar f={i.friend} noTitle />
      <b>{i.friend.name}</b>
      <span className={`inv-chip${c.tone ? ` ${c.tone}` : ''}`}>{c.text}</span>
    </div>
  )
}

function ReceivedRow({ i, now }: { i: InviteDto; now: number }) {
  const left = Math.max(1, Math.ceil((i.expires_at - now) / 60_000))
  return (
    <div className="inv-rr">
      <Avatar f={i.friend} noTitle />
      <span className="who"><b>{i.friend.name}</b><span className="sub">{i.game.name} · {left} min</span></span>
      <button className="btn play sm" onClick={() => void accept(i)}>{playWord(i)}</button>
      <button className="btn ghost sm" onClick={() => void decline(i)}>Decline</button>
    </div>
  )
}

/** Screen D: the top bar's Invites, shown while there are invites to follow, with the count. */
export function InvitesBar() {
  const can = useCanInvite()
  const list = useInvites((s) => s.invites)
  const now = useInvites((s) => s.now)
  const [open, setOpen] = useState(false)
  const box = useRef<HTMLDivElement>(null)
  const v = useMemo(() => visible(list, now), [list, now])
  useEffect(() => {
    if (!open) return
    const press = (e: PointerEvent) => { if (!box.current?.contains(e.target as Node)) setOpen(false) }
    const key = (e: KeyboardEvent) => { if (e.key === 'Escape') { e.stopPropagation(); setOpen(false) } }
    document.addEventListener('pointerdown', press, true)
    document.addEventListener('keydown', key, true)
    return () => { document.removeEventListener('pointerdown', press, true); document.removeEventListener('keydown', key, true) }
  }, [open])
  // Shown to Josh in the list: the senders see "Seen".
  useEffect(() => { if (open) markSeen(v.received) }, [open, v.received])
  useEffect(() => { if (!v.count) setOpen(false) }, [v.count])
  if (!can || !v.count) return null
  const one = v.sent.length === 1 && !v.received.length
  return (
    <div ref={box} style={{ display: 'contents' }}>
      <button className={`inv-ring${open ? ' on' : ''}`} title="Invites" aria-haspopup="dialog" aria-expanded={open} onClick={() => setOpen((o) => !o)}>
        <InviteIcon size={20} />
        <span className="cnt">{v.count}</span>
      </button>
      {open && (
        <div className="inv-pop inv-bar-pop" role="dialog" aria-label="Invites">
          <div className="inv-sh"><b>Invites</b>{one && <span className="sub">{v.sent[0].game.name}</span>}</div>
          {v.received.length > 0 && (
            <div className="inv-fl">
              <span className="inv-gh">Invited you</span>
              {v.received.map((i) => <ReceivedRow key={i.id} i={i} now={now} />)}
            </div>
          )}
          {v.sent.map((g) => {
            const again = againList(g.invites, now)
            const live = g.invites.filter((i) => isOpen(i, now))
            return (
              <div key={g.game.id} className="inv-group">
                {!one && <span className="inv-gh">{g.game.name}</span>}
                <div className="inv-fl">{g.invites.map((i) => <SentRow key={i.id} i={i} now={now} />)}</div>
                <div className="inv-foot2">
                  <button className="btn ghost sm" disabled={!again.length} onClick={() => void inviteAgain(g.invites)}>Invite again</button>
                  <button className="btn ghost sm" disabled={!live.length} onClick={() => void cancelOpen(g.invites)}>Cancel invite</button>
                </div>
              </div>
            )
          })}
        </div>
      )}
    </div>
  )
}

/** "Lerato joined" (screen D). */
export function JoinedToast() {
  const j = useInvites((s) => s.joined)
  if (!j) return null
  return (
    <div className="inv-toast" role="status">
      <Avatar f={j} noTitle />
      {j.name} joined
      <span className="ok"><CheckIcon size={12} /></span>
    </div>
  )
}

/** The invite a grapple://invite/<id> link opened: who, which game, and one Play (or why there is none). */
export function InviteOpen() {
  const o = useInvites((s) => s.open)
  const status = useInvites((s) => s.status)
  const now = useInvites((s) => s.now)
  const signedIn = useTether((s) => s.signedIn)
  const signingIn = useTether((s) => s.signingIn)
  const games = useGames((s) => s.all)
  useEffect(() => {
    if (!o) return
    const key = (e: KeyboardEvent) => { if (e.key === 'Escape') { e.stopPropagation(); closeOpen() } }
    document.addEventListener('keydown', key, true)
    return () => document.removeEventListener('keydown', key, true)
  }, [o])
  if (!o) return null
  const i = o.invite
  const lib = i?.library_game ? games.find((g) => g.id === i.library_game) ?? gameById(i.library_game) : undefined
  const art = lib ? backdrop(lib) : { url: null, blur: false }
  const st = i ? stateAt(i, Date.now()) : null
  // Play again is allowed on an accepted invite until its time is up.
  const canPlay = !!i && !i.outgoing && (st === 'sent' || st === 'seen' || (st === 'accepted' && i.expires_at > Date.now()))
  const ended = { expired: 'Expired', declined: 'Declined', cancelled: 'Cancelled', accepted: 'Joined' } as Record<string, string>
  let body: React.ReactNode
  if (!signedIn || status === 'off') {
    body = <button className="btn play" disabled={signingIn} onClick={() => void tether.signIn()}>{signingIn ? 'Waiting for the browser…' : 'Sign in with tether'}</button>
  } else if (status === 'needs_consent') {
    body = <button className="btn play" disabled={signingIn} onClick={() => void tether.signIn()}>{signingIn ? 'Waiting for the browser…' : 'Allow invites'}</button>
  } else if (status === 'unsupported') {
    body = <span className="sub">This tether has no game invites.</span>
  } else if (o.error) {
    body = <span className="sub">{o.error}</span>
  } else if (!i) {
    body = <span className="sub">Loading…</span>
  } else if (canPlay) {
    body = <button className="btn play" onClick={() => void accept(i)}>{playWord(i)}</button>
  } else {
    body = <span className="inv-chip dim">{st ? ended[st] ?? chip(i, now).text : ''}</span>
  }
  return createPortal(
    <div className="inv-scrim" onClick={closeOpen}>
      <div className="inv-card" role="dialog" aria-modal="true" aria-label="Game invite" onClick={(e) => e.stopPropagation()}>
        <div className={`art${art.blur ? ' blur' : ''}`} style={art.url ? { backgroundImage: `url("${art.url}")` } : undefined}>
          {!art.url && <InviteIcon size={34} />}
        </div>
        <button className="inv-x" title="Close (Esc)" onClick={closeOpen}><CloseIcon /></button>
        <div className="bd">
          {i && (
            <div className="from">
              <Avatar f={i.friend} noTitle size={26} />
              <span>{i.outgoing ? <>You invited <b>{i.friend.name}</b></> : <><b>{i.friend.name}</b> invited you</>}</span>
            </div>
          )}
          <div className="nm">{i?.game.name ?? 'Game invite'}</div>
          {i && canPlay && <span className="sub">{Math.max(1, Math.ceil((i.expires_at - Date.now()) / 60_000))} min left</span>}
          <div className="acts">{body}</div>
        </div>
      </div>
    </div>,
    document.body,
  )
}
