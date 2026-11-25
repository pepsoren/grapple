import { useState } from 'react'
import { tether, useTether } from '../tether'
import badge from '../assets/tether/tether-badge-squircle-white.svg'

/** Where Grapple will sign in, editable (the `tether.instance_url` setting). */
export function InstanceField({ onDone }: { onDone?: () => void }) {
  const current = useTether((s) => s.instanceUrl)
  const [value, setValue] = useState(current)
  const [error, setError] = useState<string | null>(null)
  const save = async () => {
    const err = await tether.setInstanceUrl(value)
    setError(err)
    if (!err) onDone?.()
  }
  return (
    <>
      <input
        value={value}
        placeholder="https://tether.example"
        spellCheck={false}
        onChange={(e) => setValue(e.target.value)}
        onKeyDown={(e) => { if (e.key === 'Enter') void save(); e.stopPropagation() }}
        onBlur={() => value !== current && void save()}
      />
      {error && <div className="err">{error}</div>}
    </>
  )
}

/** First run: sign in with tether (or skip). The sign-in itself happens in the system browser. */
export function SignIn() {
  const show = useTether((s) => s.loaded && !s.signedIn && !s.skipped)
  const waiting = useTether((s) => s.signingIn)
  const error = useTether((s) => s.error)
  const instance = useTether((s) => s.instanceUrl)
  const [editing, setEditing] = useState(false)
  if (!show) return null
  return (
    <div className="modal">
      <div className="signin">
        <img className="mark" src={badge} alt="tether" width={64} height={64} draggable={false} />
        <div className="t">Sign in with tether</div>
        <div className="sub">
          {waiting
            ? 'Waiting for you in the browser. Press Allow there, then come back.'
            : 'Playtime and now-playing follow your tether account. Your Steam link in tether brings your whole Steam library and history.'}
        </div>
        {waiting ? (
          <button className="btn play" onClick={() => tether.cancelSignIn()}>Cancel</button>
        ) : (
          <button className="btn play" onClick={() => void tether.signIn()}>Continue in browser</button>
        )}
        {error && <div className="err" style={{ marginTop: 10 }}>{error}</div>}
        {!waiting && instance && (
          <div className="where">
            {editing ? <InstanceField onDone={() => setEditing(false)} /> : (
              <div>tether at <b>{instance.replace(/^https?:\/\//, '')}</b> <button className="skip" onClick={() => setEditing(true)}>change</button></div>
            )}
          </div>
        )}
        {!waiting && <button className="skip" onClick={() => tether.skip()}>Skip for now</button>}
      </div>
    </div>
  )
}
