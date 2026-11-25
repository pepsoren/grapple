import { useEffect, useRef, useState } from 'react'
import { api } from '../lib/ipc'
import { addManual } from '../lib/library'
import { usePlatform } from '../lib/platform'
import { closeAddGame, useUi } from '../lib/ui'
import type { Detected } from '../lib/types'
import { CloseIcon } from './Icons'

const KINDS: [string, string][] = [
  ['windows', 'Windows program (Proton)'],
  ['appimage', 'AppImage'],
  ['native', 'Native program'],
  ['command', 'Custom command'],
]

// On Windows a program simply runs: no Proton, no AppImages.
const KINDS_WINDOWS: [string, string][] = [
  ['windows', 'Windows program'],
  ['command', 'Custom command'],
]

function Form() {
  const windows = usePlatform()?.os === 'windows'
  const [path, setPath] = useState('')
  const [name, setName] = useState('')
  const [nameEdited, setNameEdited] = useState(false)
  const [kind, setKind] = useState('auto')
  const [args, setArgs] = useState('')
  const [detected, setDetected] = useState<Detected | null>(null)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const nameRef = useRef<HTMLInputElement>(null)

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.preventDefault()
        closeAddGame()
      }
    }
    document.addEventListener('keydown', onKey)
    return () => document.removeEventListener('keydown', onKey)
  }, [])

  const take = (d: Detected) => {
    setDetected(d)
    setPath(d.path)
    if (!nameEdited) setName(d.name)
  }
  const choose = async () => {
    setError(null)
    try {
      const d = await api.pickProgram()
      if (d) {
        take(d)
        nameRef.current?.focus()
      }
    } catch (e) {
      setError(String(e))
    }
  }
  const detect = async () => {
    const p = path.trim()
    if (!p) return setDetected(null)
    try {
      take(await api.detectProgram(p))
    } catch {
      /* the add step reports problems */
    }
  }
  const submit = async () => {
    if (busy) return
    setBusy(true)
    setError(null)
    const err = await addManual({ name, path, kind, args })
    setBusy(false)
    if (err) setError(err)
    else closeAddGame()
  }
  const autoLabel = detected ? `Detected: ${detected.label}` : 'Detect from the file'

  return (
    <div className="modal" onMouseDown={(e) => e.target === e.currentTarget && closeAddGame()}>
      <form className="addgame" onSubmit={(e) => { e.preventDefault(); void submit() }}>
        <button type="button" className="btn ghost x" title="Close (Esc)" onClick={closeAddGame}><CloseIcon /></button>
        <div className="t">Add any game</div>
        <div className="sub">
          {windows ? 'A game\'s .exe (or .bat), or any command.' : 'A Windows .exe (through Proton), an AppImage, a native program or any command.'}
        </div>
        <label className="fld">
          <span>Program</span>
          <div className="pick">
            <input
              value={path}
              placeholder={windows ? 'C:\\Games\\Game\\game.exe, or a command' : '/path/to/game.exe, or a command like retroarch'}
              spellCheck={false}
              onChange={(e) => { setPath(e.target.value); setDetected(null) }}
              onBlur={() => void detect()}
            />
            <button type="button" className="btn ghost" onClick={() => void choose()}>Choose file…</button>
          </div>
        </label>
        <label className="fld">
          <span>Name</span>
          <input ref={nameRef} value={name} placeholder="Shown in your library" onChange={(e) => { setName(e.target.value); setNameEdited(true) }} />
        </label>
        <label className="fld">
          <span>Runs as</span>
          <select value={kind} onChange={(e) => setKind(e.target.value)}>
            <option value="auto">{autoLabel}</option>
            {(windows ? KINDS_WINDOWS : KINDS).map(([k, label]) => <option key={k} value={k}>{label}</option>)}
          </select>
        </label>
        <label className="fld">
          <span>Arguments <small>optional</small></span>
          <input value={args} placeholder="-windowed -fullscreen" spellCheck={false} onChange={(e) => setArgs(e.target.value)} />
        </label>
        {error && <div className="err">{error}</div>}
        <div className="acts">
          <button type="button" className="skip" onClick={closeAddGame}>Cancel</button>
          <button type="submit" className="btn play" disabled={busy || !path.trim() || !name.trim()}>Add game</button>
        </div>
      </form>
    </div>
  )
}

export function AddGame() {
  const open = useUi((s) => s.addGame)
  return open ? <Form /> : null
}
