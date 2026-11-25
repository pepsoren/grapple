// Connected controllers (src-tauri/src/controllers/), which input was used last, and the button prompts that follow
// from both. The prompt logic is pure so it can be checked without a pad.
import { api, events } from './ipc'
import { settingsStore, useSettings } from './settings'
import { createStore, useStore } from './store'
import type { PadAction, PadInfo, PadLayout, PadPrompts } from './types'

interface PadsState {
  list: PadInfo[]
  loaded: boolean
  /** The last input came from a pad (pad-nav), not the keyboard or the pointer. */
  padLast: boolean
  /** That pad's id and layout (for "Auto" prompts and the confirm button). */
  pad: string | null
  layout: PadLayout
}

export const padsStore = createStore<PadsState>({ list: [], loaded: false, padLast: false, pad: null, layout: 'xbox' })
export const usePads = <S>(select: (s: PadsState) => S) => useStore(padsStore, select)

let started = false
/** Load the list once and follow plugs and unplugs. */
export function startPads() {
  if (started) return
  started = true
  void events.controllersChanged((list) => padsStore.set({ list, loaded: true }))
  void refreshPads()
}

export async function refreshPads() {
  try {
    padsStore.set({ list: await api.controllersList(), loaded: true })
  } catch {
    padsStore.set({ loaded: true })
  }
}

/** A face, shoulder or menu button, by position. */
export type PadPos = 'south' | 'east' | 'west' | 'north' | 'start' | 'lb' | 'rb'

/** Where an action sits on a pad: Nintendo pads confirm with the right button, every other pad with the bottom one. */
export function position(action: PadAction, padLayout: PadLayout): PadPos | null {
  const nin = padLayout === 'nintendo'
  switch (action) {
    case 'confirm': return nin ? 'east' : 'south'
    case 'back': return nin ? 'south' : 'east'
    case 'alt': return 'west'
    case 'search': return 'north'
    case 'menu': return 'start'
    case 'prev': return 'lb'
    case 'next': return 'rb'
    default: return null
  }
}

/** Which maker's prompts to draw: the setting, or (Auto) the pad's own. */
export function promptStyle(setting: PadPrompts, padLayout: PadLayout): PadLayout {
  return setting === 'auto' ? padLayout : setting
}

export type Glyph = { kind: 'letter'; text: string } | { kind: 'shape'; shape: 'cross' | 'circle' | 'square' | 'triangle' } | { kind: 'word'; text: string }

/** What is printed on the button at `pos` on that maker's pads. */
export function glyph(style: PadLayout, pos: PadPos): Glyph {
  const face: Record<PadLayout, Record<'south' | 'east' | 'west' | 'north', Glyph>> = {
    xbox: { south: { kind: 'letter', text: 'A' }, east: { kind: 'letter', text: 'B' }, west: { kind: 'letter', text: 'X' }, north: { kind: 'letter', text: 'Y' } },
    nintendo: { south: { kind: 'letter', text: 'B' }, east: { kind: 'letter', text: 'A' }, west: { kind: 'letter', text: 'Y' }, north: { kind: 'letter', text: 'X' } },
    playstation: { south: { kind: 'shape', shape: 'cross' }, east: { kind: 'shape', shape: 'circle' }, west: { kind: 'shape', shape: 'square' }, north: { kind: 'shape', shape: 'triangle' } },
  }
  switch (pos) {
    case 'south': case 'east': case 'west': case 'north': return face[style][pos]
    case 'start': return { kind: 'word', text: style === 'playstation' ? 'Options' : style === 'nintendo' ? '+' : 'Menu' }
    case 'lb': return { kind: 'word', text: style === 'playstation' ? 'L1' : style === 'nintendo' ? 'L' : 'LB' }
    case 'rb': return { kind: 'word', text: style === 'playstation' ? 'R1' : style === 'nintendo' ? 'R' : 'RB' }
  }
}

/** The glyph for an action right now, or null while the keyboard or pointer was used last. */
export function usePadGlyph(action: PadAction): { glyph: Glyph; style: PadLayout } | null {
  const padLast = usePads((s) => s.padLast)
  const layout = usePads((s) => s.layout)
  const setting = useSettings((s) => s.pad_prompts)
  if (!padLast) return null
  const pos = position(action, layout)
  if (!pos) return null
  const style = promptStyle(setting ?? 'auto', layout)
  return { glyph: glyph(style, pos), style }
}

/** The pad was used (pad-nav). */
export function notePad(pad: string, layout: PadLayout) {
  padsStore.set({ padLast: true, pad, layout })
  document.documentElement.dataset.input = 'pad'
}

/** The keyboard or pointer was used: prompts and the focus ring go back to normal. */
export function noteOther() {
  if (!padsStore.get().padLast) return
  padsStore.set({ padLast: false })
  delete document.documentElement.dataset.input
}

/** "Vibrate on focus": a tiny tick on the pad that moved the focus, when it can rumble. */
export function focusTick() {
  const { pad, list } = padsStore.get()
  if (!pad || !settingsStore.get().pad_vibrate) return
  if (list.length && !list.find((p) => p.id === pad)?.rumble) return
  void api.controllerRumble(pad, 'tick').catch(() => {})
}
