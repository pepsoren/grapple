// Settings › Accounts: tether (sign-in and address), and the stores as grapple sees them locally.
import { useEffect, useState } from 'react'
import { api } from '../../lib/ipc'
import type { AccountsInfo } from '../../lib/types'
import { toast } from '../../lib/ui'
import { tether, useTether } from '../../tether'
import { Avatar } from '../Avatar'
import { Confirm } from '../downloads/parts'
import { InstanceField } from '../SignIn'
import { EpicMark, SteamMark } from './icons'
import { Btn, Group, Match, Pane, Row } from './kit'

const host = (url: string) => url.replace(/^https?:\/\//, '').replace(/\/$/, '')

function TetherCard() {
  const t = useTether((s) => s)
  const [editing, setEditing] = useState(false)
  return (
    <Group title="grapple">
      <Match label="tether sign in sign out account">
        <div className="set-acct">
          <Avatar f={{ ...t.me, name: t.me.name || 'You' }} size={44} noTitle />
          <div>
            <b>{t.signedIn ? t.me.name || 'tether' : 'tether'}</b>
            <div className="st">
              {t.signedIn
                ? <><span className="ok">Signed in with tether</span>{t.instanceUrl && ` · ${host(t.instanceUrl)}`}</>
                : t.signingIn ? 'Waiting for the browser' : 'Not signed in'}
            </div>
          </div>
          {t.signedIn
            ? <Btn onClick={() => void tether.signOut()}>Sign out</Btn>
            : t.signingIn ? <Btn onClick={() => tether.cancelSignIn()}>Cancel</Btn> : <Btn onClick={() => void tether.signIn()}>Sign in</Btn>}
        </div>
      </Match>
      {t.instanceUrl && (
        <Row label="tether address" keywords="server instance url">
          {editing
            ? <div className="set-inline"><InstanceField onDone={() => setEditing(false)} /></div>
            : <button className="set-pill mono" onClick={() => setEditing(true)}>{host(t.instanceUrl)} ▾</button>}
        </Row>
      )}
      {t.error && <div className="set-err">{t.error}</div>}
    </Group>
  )
}

function Stores() {
  const [a, setA] = useState<AccountsInfo | null>(null)
  const [asking, setAsking] = useState(false)
  const load = () => api.accounts().then(setA, () => setA(null))
  useEffect(() => { void load() }, [])
  if (!a) return null
  return (
    <Group title="Stores">
      <Match label="Steam store sign out library account">
        <div className="set-acct">
          <span className="set-lg"><SteamMark size={28} /></span>
          <div>
            <b>Steam{a.steam_name && ` · ${a.steam_name}`}</b>
            <div className="st">{a.steam_found ? <><span className="ok">Library</span> from the Steam app</> : 'Steam not found'}</div>
          </div>
          {a.store_data ? <Btn onClick={() => setAsking(true)}>Sign out of store</Btn> : <span />}
        </div>
      </Match>
      <Match label="Epic Games account store library">
        <div className="set-acct">
          <span className="set-lg"><EpicMark size={26} /></span>
          <div>
            <b>Epic Games{a.epic_name && ` · ${a.epic_name}`}</b>
            <div className="st">{a.epic_signed_in ? <><span className="ok">Library</span> and <span className="ok">Store</span> signed in</> : 'Not signed in'}</div>
          </div>
          <span />
        </div>
      </Match>
      {asking && (
        <Confirm
          title="Sign out of the store?"
          text="The store window forgets your Steam and Epic sign-ins."
          yes="Sign out"
          onNo={() => setAsking(false)}
          onYes={() => {
            setAsking(false)
            api.storeSignOut().then(() => { toast('Signed out of the store'); void load() }, (e) => toast(String(e)))
          }}
        />
      )}
    </Group>
  )
}

export function AccountsPane() {
  return (
    <Pane title="Accounts">
      <TetherCard />
      <Stores />
    </Pane>
  )
}
