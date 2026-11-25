// Uninstall on a game page (docs/DOWNLOADS_PLAN.md: uninstall_game, after a confirm). Epic: grapple deletes the
// game's folder and its legendary entry; Steam: Steam's own uninstall.
import { useState } from 'react'
import { api } from '../../lib/ipc'
import type { Game } from '../../lib/types'
import { toast } from '../../lib/ui'
import { Confirm } from './parts'
import '../../styles/downloads.css'

export function UninstallButton({ g }: { g: Game }) {
  const [asking, setAsking] = useState(false)
  if (g.store === 'manual' || !g.installed) return null
  return (
    <>
      <button className="dl-btn dl-ghost" onClick={() => setAsking(true)}>Uninstall</button>
      {asking && (
        <Confirm
          title={`Uninstall ${g.name}?`}
          yes="Uninstall"
          onNo={() => setAsking(false)}
          onYes={() => {
            setAsking(false)
            api.uninstallGame(g.id).then(() => toast(`Uninstalling ${g.name}`), (e) => toast(String(e)))
          }}
        />
      )}
    </>
  )
}
