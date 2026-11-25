// Navigating grapple with a controller. Rust reads the pad and sends `pad-nav` actions (src-tauri/src/controllers,
// only while grapple's window is in front and no game is); this file moves the focus: the nearest focusable element
// in the pressed direction, inside the topmost open layer (palette, menu, dialog, drawer) or the page.
// Where a page already has keys (Home's shelf and Enter to play, the palette, the Collection list, Escape) the
// action becomes that key, so both inputs do the same thing.
import { events } from './ipc'
import { focusTick, noteOther, notePad, startPads } from './pads'
import { expand, sessionStore } from './session'
import { settingsStore } from './settings'
import { gamesStore } from './games'
import { openGameMenu, openPalette, uiStore } from './ui'
import type { PadAction, PadNav } from './types'
import '../styles/controllers.css'

export type Dir = 'up' | 'down' | 'left' | 'right'
export interface Box { left: number; top: number; right: number; bottom: number }

/** The candidate to move to from `cur` in `dir`, or -1. Pure: boxes in, index out.
 *  A candidate must lie further that way (its centre past ours and its far edge past ours). The nearest wins,
 *  with a strong preference for one that lines up with `cur` (overlaps it across the direction). */
export function pickNext(cur: Box, cands: Box[], dir: Dir): number {
  const cx = (cur.left + cur.right) / 2
  const cy = (cur.top + cur.bottom) / 2
  const horiz = dir === 'left' || dir === 'right'
  let best = -1
  let bestScore = Infinity
  cands.forEach((c, i) => {
    const x = (c.left + c.right) / 2
    const y = (c.top + c.bottom) / 2
    const ahead =
      dir === 'right' ? x > cx + 0.5 && c.right > cur.right
        : dir === 'left' ? x < cx - 0.5 && c.left < cur.left
          : dir === 'down' ? y > cy + 0.5 && c.bottom > cur.bottom
            : y < cy - 0.5 && c.top < cur.top
    if (!ahead) return
    const gap = Math.max(0,
      dir === 'right' ? c.left - cur.right : dir === 'left' ? cur.left - c.right : dir === 'down' ? c.top - cur.bottom : cur.top - c.bottom)
    const overlap = horiz ? Math.min(cur.bottom, c.bottom) - Math.max(cur.top, c.top) : Math.min(cur.right, c.right) - Math.max(cur.left, c.left)
    const apart = Math.max(0, -overlap)
    const offset = horiz ? Math.abs(y - cy) : Math.abs(x - cx)
    const score = gap + apart * 4 + offset * 0.1
    if (score < bestScore) {
      bestScore = score
      best = i
    }
  })
  return best
}

/** The next / previous page in the top bar's order (wraps). `cur` -1: none is on. */
export function cycle(cur: number, n: number, step: 1 | -1): number {
  if (n <= 0) return -1
  if (cur < 0) return step === 1 ? 0 : n - 1
  return (cur + step + n) % n
}

/** Clickable cards and rows that are not buttons. */
const CARDS = '.lib .grid .card, .clist .crow, .crow.home, .prow'
const FOCUSABLE = [
  'button:not([disabled])', 'a[href]', 'input:not([disabled]):not([type="hidden"])', 'select', 'textarea',
  '[tabindex]', '[role="button"]', '[role="menuitem"]', '[role="switch"]', '[role="radio"]', '[role="tab"]',
  CARDS, '[data-pad]',
].join(',')
/** Open layers, topmost last in the document: the focus stays inside the last visible one. */
const LAYERS = '.pal, [role="dialog"], [aria-modal="true"], .modal, [role="menu"], .drawer.open, .dl-pop, .menu'

function visible(el: Element): boolean {
  const r = el.getBoundingClientRect()
  if (r.width < 2 || r.height < 2) return false
  const cs = getComputedStyle(el)
  return cs.visibility !== 'hidden' && cs.display !== 'none' && Number(cs.opacity) > 0.05
}

function scope(): Element {
  const layers = [...document.querySelectorAll(LAYERS)].filter(visible)
  return layers[layers.length - 1] ?? document.body
}

function candidates(root: Element): HTMLElement[] {
  return [...root.querySelectorAll<HTMLElement>(FOCUSABLE)].filter((el) => {
    if (el.closest('[inert], [aria-hidden="true"]')) return false
    // tabindex -1 opts out (e.g. the "coming later" Stats tab), unless we gave it ourselves.
    if (el.getAttribute('tabindex') === '-1' && !el.hasAttribute('data-padtab')) return false
    // A card's own buttons (the Play on a library card) are reached through the card.
    if (el.parentElement?.closest(CARDS)) return false
    // Layers below the current one are not reachable.
    if (root === document.body && el.closest(LAYERS)) return false
    return visible(el)
  })
}

const box = (el: Element): Box => {
  const r = el.getBoundingClientRect()
  return { left: r.left, top: r.top, right: r.right, bottom: r.bottom }
}

let current: HTMLElement | null = null
/** The last element focused in each layer, to come back to when a layer above it closes. */
const lastIn = new WeakMap<Element, HTMLElement>()

function setFocus(el: HTMLElement, root: Element) {
  current?.classList.remove('pad-focus')
  current = el
  lastIn.set(root, el)
  if (!el.matches('button, a[href], input, select, textarea, [tabindex]')) {
    el.setAttribute('tabindex', '-1')
    el.setAttribute('data-padtab', '')
  }
  el.classList.add('pad-focus')
  el.focus({ preventScroll: true })
  const smooth = !settingsStore.get().reduce_motion
  el.scrollIntoView({ block: 'nearest', inline: 'nearest', behavior: smooth ? 'smooth' : 'auto' })
  // Moving onto a Home shelf tile or a Collection row selects it, as the arrow keys do.
  if (el.matches('.shelf .tile, .clist .crow, .crow.home')) el.click()
}

/** Where a move starts: the focused element if it is in the layer, else the one last focused there, else the
 *  selected item, else the first. */
function start(root: Element, list: HTMLElement[]): HTMLElement | null {
  const act = document.activeElement as HTMLElement | null
  if (act && list.includes(act)) return act
  if (current && current.isConnected && list.includes(current)) return current
  const last = lastIn.get(root)
  if (last?.isConnected && list.includes(last)) return last
  return list.find((el) => el.matches('.on, [aria-checked="true"], [aria-selected="true"]')) ?? list[0] ?? null
}

function key(k: string, target?: Element | null) {
  const el = target ?? (document.activeElement as HTMLElement | null) ?? document.body
  el.dispatchEvent(new KeyboardEvent('keydown', { key: k, bubbles: true, cancelable: true }))
}

/** A text field that takes the pad's directions as arrow keys (the palette's search). */
const arrowField = (el: Element | null) => !!el?.matches('.pal input')

function move(dir: Dir) {
  const act = document.activeElement
  if (arrowField(act)) {
    if (dir === 'up' || dir === 'down') key(dir === 'up' ? 'ArrowUp' : 'ArrowDown', act)
    return
  }
  const root = scope()
  const list = candidates(root)
  const from = start(root, list)
  if (!from) return
  // The first press only shows where the focus is.
  if (from !== current || !from.classList.contains('pad-focus')) {
    setFocus(from, root)
    return
  }
  const others = list.filter((el) => el !== from)
  const i = pickNext(box(from), others.map(box), dir)
  if (i < 0) return
  setFocus(others[i], root)
  focusTick()
}

/** After an action opens or closes a layer, put the focus where it belongs. */
function settle() {
  requestAnimationFrame(() => requestAnimationFrame(() => {
    const root = scope()
    const list = candidates(root)
    if (current && current.isConnected && list.includes(current)) return
    const to = start(root, list)
    if (to) setFocus(to, root)
  }))
}

function confirm() {
  const el = (document.activeElement as HTMLElement | null) ?? null
  if (!el || el === document.body) {
    // Nothing focused yet: the first press only shows where the focus is (never starts a game by itself).
    const root = scope()
    const to = start(root, candidates(root))
    if (to) setFocus(to, root)
    return
  }
  if (arrowField(el)) return key('Enter', el)
  if (el.matches('.shelf .tile, .clist .crow')) return key('Enter', el)
  if (el.matches('.lib .grid .card')) {
    // Select it (Home scrolls up to it), then the focus goes to its Play button.
    el.click()
    requestAnimationFrame(() => {
      const play = document.querySelector<HTMLElement>('.hero .actions .btn.play')
      if (play) setFocus(play, scope())
    })
    return
  }
  if (el.matches('input, textarea')) return
  el.click()
  settle()
}

function gameOf(el: Element | null): string | null {
  const host = el?.closest<HTMLElement>('[data-game], .clist .crow[data-c]')
  return host?.dataset.game ?? host?.dataset.c ?? (uiStore.get().route === 'home' ? gamesStore.get().selected : null)
}

function act(a: PadAction) {
  const ui = uiStore.get()
  switch (a) {
    case 'up': case 'down': case 'left': case 'right': return move(a)
    case 'confirm': return confirm()
    case 'back':
      key('Escape')
      return settle()
    case 'alt':
      key('Tab')
      return settle()
    case 'search':
      if (ui.route === 'collection') {
        document.querySelector<HTMLInputElement>('.csearch input')?.focus()
        return
      }
      openPalette()
      return
    case 'menu': {
      const id = gameOf(document.activeElement)
      if (!id) return
      const r = (current ?? document.activeElement ?? document.body).getBoundingClientRect()
      const at = { preventDefault() {}, stopPropagation() {}, clientX: r.left + r.width / 2, clientY: r.top + r.height / 2 }
      openGameMenu(at, id)
      return settle()
    }
    case 'prev': case 'next': {
      if (scope() !== document.body) return
      const navs = [...document.querySelectorAll<HTMLElement>('.bar .nav')].filter((n) => !n.classList.contains('dim'))
      const i = cycle(navs.findIndex((n) => n.classList.contains('on')), navs.length, a === 'next' ? 1 : -1)
      navs[i]?.click()
      current?.classList.remove('pad-focus')
      current = null
      return
    }
  }
}

function onNav(n: PadNav) {
  if (!settingsStore.get().pad_nav) return
  notePad(n.pad, n.layout)
  // The playing card: only Back, which brings the full window back (as Escape does).
  if (sessionStore.get().compact) {
    if (n.action === 'back') expand()
    return
  }
  act(n.action)
}

let started = false
/** Listen for pad-nav (once). The keyboard or pointer takes over as soon as it is used. */
export function startPadNav() {
  if (started) return
  started = true
  startPads()
  void events.padNav(onNav)
  const other = (e: Event) => {
    if (!e.isTrusted) return
    noteOther()
    current?.classList.remove('pad-focus')
  }
  document.addEventListener('keydown', other, true)
  document.addEventListener('pointerdown', other, true)
  document.addEventListener('wheel', other, { capture: true, passive: true })
}
