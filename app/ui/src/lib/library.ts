// Actions on the library that go beyond playing: install, add and remove games.
import { installGame as openInstallSheet } from './downloads'
import { refreshGames, select } from './games'
import { api } from './ipc'
import { toast, setDrawer } from './ui'
import type { Game, ManualDraft } from './types'

/** "Install" on a game that is not installed: the install sheet (drive, size, Proton), then the download queue. */
export function installGame(g: Game) {
  if (g.store === 'manual') return
  openInstallSheet(g)
}

/** Add a program by hand. Resolves with an error text, or null after selecting the new game. */
export async function addManual(draft: ManualDraft): Promise<string | null> {
  try {
    const id = await api.addManualGame(draft)
    await refreshGames()
    select(id)
    toast(`Added ${draft.name.trim()}`)
    return null
  } catch (e) {
    return String(e)
  }
}

export async function removeManual(g: Game) {
  if (g.store !== 'manual') return
  try {
    await api.removeManualGame(g.id)
    setDrawer(false)
    await refreshGames()
    toast(`Removed ${g.name} from grapple`)
  } catch (e) {
    toast(String(e))
  }
}
