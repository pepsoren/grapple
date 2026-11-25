// Settings › Friends & tether: what friends see (now playing, per game) and friends on Home.
import { useEffect, useMemo, useState } from 'react'
import { useGames } from '../../lib/games'
import { api } from '../../lib/ipc'
import { updateSettings, useSettings } from '../../lib/settings'
import { toast } from '../../lib/ui'
import { useInvites } from '../../lib/invites'
import { tether, useTether } from '../../tether'
import { Btn, Choice, Dialog, Group, Pane, Row, Switch } from './kit'

/** Set one game's "Show it to friends when I play" (its other choices stay). */
export async function setShared(id: string, shared: boolean) {
  const gs = await api.getGameSettings(id)
  await api.setGameSettings(id, { ...gs, show_in_tether: shared })
}

function Chooser({ notShared, onChange, onClose }: { notShared: Set<string>; onChange: (id: string, hide: boolean) => void; onClose: () => void }) {
  const all = useGames((s) => s.all)
  const [q, setQ] = useState('')
  // Not shared first, then by name. The order is kept while the list is open, so rows do not jump.
  const [first] = useState(notShared)
  const list = useMemo(
    () => [...all].sort((a, b) => Number(first.has(b.id)) - Number(first.has(a.id)) || a.sort_name.localeCompare(b.sort_name)),
    [all, first],
  )
  const ql = q.trim().toLowerCase()
  return (
    <Dialog title="Games I don't share" onClose={onClose}>
      <input className="set-input wide" autoFocus value={q} placeholder="Find a game" spellCheck={false} onChange={(e) => setQ(e.target.value)} onKeyDown={(e) => e.stopPropagation()} />
      {list.filter((g) => !ql || g.name.toLowerCase().includes(ql)).map((g) => (
        <div key={g.id} className="set-li">
          <span>{g.name}</span>
          <Switch on={notShared.has(g.id)} label={`Don't share ${g.name}`} onChange={(v) => onChange(g.id, v)} />
        </div>
      ))}
    </Dialog>
  )
}

type From = 'friends' | 'nobody'

/** Mockup F's rows that v1 really has: "Who can invite me" (kept by tether, also in tether's Settings › Privacy).
 *  Shown while signed in to a tether with invites; before the sign-in has allowed them, "Allow invites". */
function Invites() {
  const status = useInvites((s) => s.status)
  const signedIn = useTether((s) => s.signedIn)
  const signingIn = useTether((s) => s.signingIn)
  const [from, setFrom] = useState<From | null>(null)
  useEffect(() => {
    if (status === 'ready') api.inviteSettingsGet().then(setFrom, () => setFrom(null))
  }, [status])
  if (!signedIn || (status !== 'ready' && status !== 'needs_consent')) return null
  const change = (v: From) => {
    const before = from
    setFrom(v)
    api.inviteSettingsSet(v).then(setFrom, (e) => { setFrom(before); toast(String(e)) })
  }
  return (
    <Group title="Invites">
      <Row label="Who can invite me" keywords="game invites play">
        {status === 'needs_consent'
          ? <Btn disabled={signingIn} onClick={() => void tether.signIn()}>Allow invites</Btn>
          : <Choice value={from} options={[['friends', 'Friends'], ['nobody', 'Nobody']]} onChange={(v) => v && change(v)} label={from ? undefined : '…'} />}
      </Row>
    </Group>
  )
}

export function SocialPane() {
  const s = useSettings((x) => x)
  const names = useGames((x) => x.all)
  const [notShared, setNotShared] = useState<Set<string> | null>(null)
  const [choosing, setChoosing] = useState(false)
  useEffect(() => { api.gamesNotShared().then((ids) => setNotShared(new Set(ids)), () => setNotShared(new Set())) }, [])
  const change = (id: string, hide: boolean) => {
    setNotShared((cur) => {
      const next = new Set(cur)
      if (hide) next.add(id)
      else next.delete(id)
      return next
    })
    setShared(id, !hide).catch((e) => toast(String(e)))
  }
  const count = notShared?.size ?? 0
  const one = count === 1 ? names.find((g) => notShared?.has(g.id))?.name : undefined
  return (
    <Pane title="Friends & tether">
      <Group title="What friends see">
        <Row label="Show what I play" keywords="now playing presence">
          <Switch on={s.show_in_tether} onChange={(v) => void updateSettings({ show_in_tether: v })} />
        </Row>
      </Group>
      <Group title="What I see">
        <Row label="Friends playing, on Home">
          <Switch on={s.friends_on_home} onChange={(v) => void updateSettings({ friends_on_home: v })} />
        </Row>
      </Group>
      <Group title="Per game">
        <Row label="Games I don't share" keywords="hide private">
          <span className="sub">{!notShared ? '' : count === 0 ? 'None' : one ?? count}</span>
          <Btn disabled={!notShared} onClick={() => setChoosing(true)}>Choose</Btn>
        </Row>
      </Group>
      <Invites />
      {choosing && notShared && <Chooser notShared={notShared} onChange={change} onClose={() => setChoosing(false)} />}
    </Pane>
  )
}
