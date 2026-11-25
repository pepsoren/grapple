// Which system grapple runs on and its version, asked once (Settings hides the rows a system does not have).
import { api } from './ipc'
import { createStore, useStore } from './store'
import type { PlatformInfo } from './types'

const platformStore = createStore<{ info: PlatformInfo | null }>({ info: null })

let asked = false
function ask() {
  if (asked) return
  asked = true
  api.platform().then((info) => platformStore.set({ info }), () => platformStore.set({ info: { os: 'linux', version: '' } }))
}

export function usePlatform(): PlatformInfo | null {
  ask()
  return useStore(platformStore, (s) => s.info)
}

/** Linux-only rows (Proton, GameMode, MangoHud, the game runtime). False until the answer is in. */
export function useLinux(): boolean {
  return usePlatform()?.os === 'linux'
}

/** Rows grapple has on Linux and Windows but not (yet) on macOS: start at login, notifications. */
export function useLinuxOrWindows(): boolean {
  const os = usePlatform()?.os
  return os === 'linux' || os === 'windows'
}
