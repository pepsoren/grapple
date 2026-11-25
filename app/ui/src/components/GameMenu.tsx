// A game's right-click menu (Home tiles and cards, Collection rows): play or install, details (where the art can
// be changed), settings, check for update, favourite, uninstall.
import { useEffect, useLayoutEffect, useRef, useState } from 'react'
import { createPortal } from 'react-dom'
import { select, toggleFavourite, useGames } from '../lib/games'
import { api } from '../lib/ipc'
import { installGame, removeManual } from '../lib/library'
import { play } from '../lib/session'
import type { Game } from '../lib/types'
import { uiZoom } from '../lib/theme'
import { closeGameMenu, openGameSheet, setDrawer, toast, uiStore, useUi } from '../lib/ui'
import { Confirm } from './downloads/parts'
import { invitePickerAt, useCanInvite } from './Invites'

const UPDATE_WORD = {
  upToDate: 'is up to date',
  available: 'has an update',
  queued: 'is queued to update',
  downloading: 'is updating',
} as const

async function checkUpdate(g: Game) {
  toast(`Checking ${g.name}…`)
  try {
    toast(`${g.name} ${UPDATE_WORD[await api.checkUpdate(g.id)]}`)
  } catch (e) {
    toast(String(e))
  }
}

export function GameMenu() {
  const m = useUi((s) => s.gameMenu)
  const g = useGames((s) => (m ? s.all.find((x) => x.id === m.id) : undefined))
  const [uninstall, setUninstall] = useState<Game | null>(null)
  const canInvite = useCanInvite()
  const ref = useRef<HTMLDivElement>(null)
  const [pos, setPos] = useState<{ x: number; y: number } | null>(null)

  // Open at the pointer, kept inside the window. With the interface size (a page zoom) the menu's own pixels are
  // zoomed too: pointer and window sizes are divided by it.
  useLayoutEffect(() => {
    if (!m || !ref.current) return setPos(null)
    const z = uiZoom()
    const w = ref.current.offsetWidth
    const h = ref.current.offsetHeight
    setPos({ x: Math.min(m.x / z, window.innerWidth / z - w - 8), y: Math.min(m.y / z, window.innerHeight / z - h - 8) })
  }, [m])
  useEffect(() => {
    if (!m) return
    const key = (e: KeyboardEvent) => { if (e.key === 'Escape') { e.stopPropagation(); closeGameMenu() } }
    document.addEventListener('keydown', key, true)
    window.addEventListener('blur', closeGameMenu)
    window.addEventListener('resize', closeGameMenu)
    return () => {
      document.removeEventListener('keydown', key, true)
      window.removeEventListener('blur', closeGameMenu)
      window.removeEventListener('resize', closeGameMenu)
    }
  }, [m])

  const store = g && g.store !== 'manual'
  const act = (f: () => void) => () => { closeGameMenu(); f() }
  return (
    <>
      {m && g && createPortal(
        <div className="gmenu-scrim" onPointerDown={closeGameMenu} onContextMenu={(e) => { e.preventDefault(); closeGameMenu() }} onWheel={closeGameMenu}>
          <div
            ref={ref}
            className="gmenu"
            role="menu"
            style={{ left: (pos ?? m).x, top: (pos ?? m).y, visibility: pos ? 'visible' : 'hidden' }}
            onPointerDown={(e) => e.stopPropagation()}
          >
            {g.installed
              ? <button role="menuitem" onClick={act(() => play(g.id))}>Play</button>
              : store && <button role="menuitem" onClick={act(() => installGame(g))}>Install</button>}
            {canInvite && <button role="menuitem" onClick={act(() => invitePickerAt(g.id, m.x, m.y))}>Invite…</button>}
            <button role="menuitem" onClick={act(() => { uiStore.set({ route: 'home' }); select(g.id); setDrawer(true) })}>Details</button>
            <button role="menuitem" onClick={act(() => openGameSheet(g.id))}>Settings</button>
            {g.installed && store && <button role="menuitem" onClick={act(() => void checkUpdate(g))}>Check for update</button>}
            <button role="menuitem" onClick={act(() => void toggleFavourite(g.id))}>{g.favourite ? 'Remove from favourites' : 'Add to favourites'}</button>
            {g.installed && store && (
              <>
                <hr />
                <button role="menuitem" className="danger" onClick={act(() => setUninstall(g))}>Uninstall</button>
              </>
            )}
            {g.store === 'manual' && (
              <>
                <hr />
                <button role="menuitem" className="danger" onClick={act(() => void removeManual(g))}>Remove from grapple</button>
              </>
            )}
          </div>
        </div>,
        document.body,
      )}
      {uninstall && (
        <Confirm
          title={`Uninstall ${uninstall.name}?`}
          yes="Uninstall"
          onNo={() => setUninstall(null)}
          onYes={() => {
            const u = uninstall
            setUninstall(null)
            api.uninstallGame(u.id).then(() => toast(`Uninstalling ${u.name}`), (e) => toast(String(e)))
          }}
        />
      )}
    </>
  )
}
