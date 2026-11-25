import { api } from './ipc'
import { createStore, useStore } from './store'
import { applyLayout, applyTheme } from './theme'
import type { Settings } from './types'

export const defaultSettings: Settings = {
  autostart: false, keep_steam_ready: false, update_runtime: true, show_in_tether: true, accent: 'green', look: 'default',
  start_hidden: true, open_on: 'home', close_to_tray: true, on_game_start: 'card', on_game_exit: 'show', ui_scale: 100,
  reduce_motion: false, hidden_stores: [], show_uninstalled: true, hidden_games: [], week_start: 'monday', close_steam: true,
  default_proton: null, default_gamemode: true, default_mangohud: false, friends_on_home: true, quiet_while_playing: true,
  pad_nav: true, pad_prompts: 'auto', pad_vibrate: false, pad_in_games: 'as_is',
}
export const settingsStore = createStore<Settings>(defaultSettings)
export const useSettings = <S>(select: (s: Settings) => S) => useStore(settingsStore, select)

export async function loadSettings() {
  const s = { ...defaultSettings, ...(await api.getSettings()) }
  settingsStore.set(s)
  applyTheme(s.accent, s.look)
  applyLayout(s.ui_scale, s.reduce_motion)
}

export async function updateSettings(patch: Partial<Settings>) {
  const next = { ...settingsStore.get(), ...patch }
  settingsStore.set(next)
  if (patch.accent || patch.look) applyTheme(next.accent, next.look)
  if (patch.ui_scale !== undefined || patch.reduce_motion !== undefined) applyLayout(next.ui_scale, next.reduce_motion)
  try {
    settingsStore.set({ ...defaultSettings, ...(await api.setSettings(next)) })
  } catch {
    settingsStore.set(settingsStore.get())
  }
}

/** "Reset settings": the backend's defaults, applied at once. */
export async function resetSettings() {
  const s = { ...defaultSettings, ...(await api.settingsReset()) }
  settingsStore.set(s)
  applyTheme(s.accent, s.look)
  applyLayout(s.ui_scale, s.reduce_motion)
}
