// data-accent / data-look on <html>, set before the first paint, and the user's theme.css override.
import { api, inTauri } from './ipc'

const KEY = 'grapple.theme'
export const ACCENTS = [
  'violet', 'indigo', 'blue', 'sky', 'teal', 'green', 'lime', 'yellow', 'amber', 'orange', 'rose', 'pink', 'fuchsia', 'grey',
] as const
export const LOOKS = ['default', 'midnight', 'dim'] as const

export function applyTheme(accent: string, look: string) {
  const el = document.documentElement
  el.dataset.theme = 'dark'
  el.dataset.accent = accent
  if (look && look !== 'default') el.dataset.look = look
  else delete el.dataset.look
  try {
    localStorage.setItem(KEY, JSON.stringify({ accent, look }))
  } catch {
    /* fine: the saved settings re-apply it a moment after start */
  }
  syncWindowBg()
}

let probe: HTMLElement | null = null
let lastBg = ''

/** Give the window itself the page's canvas colour (--window-bg), so a tile WebKit has not painted yet shows the
 *  canvas, never the desktop behind the window. Read from the tokens, so it follows the look and theme.css. */
export function syncWindowBg() {
  if (!inTauri) return
  if (!probe) {
    probe = document.createElement('div')
    probe.style.cssText = 'position:absolute;width:0;height:0;visibility:hidden;background-color:var(--window-bg)'
    document.documentElement.appendChild(probe)
  }
  const css = getComputedStyle(probe).backgroundColor
  if (css === lastBg) return
  // "rgb(12, 12, 14)" or, for colour functions, "color(srgb 0.047 0.047 0.055)".
  const n = (css.match(/[\d.]+/g) ?? []).map(Number)
  if (n.length < 3) return
  const rgb = css.startsWith('color(') ? n.slice(0, 3).map((x) => Math.round(x * 255)) : n.slice(0, 3).map(Math.round)
  lastBg = css
  void api.setWindowBg(rgb[0], rgb[1], rgb[2]).catch(() => {})
}

const LAYOUT_KEY = 'grapple.layout'
export const UI_SCALES = [90, 100, 110, 125] as const

/** The smallest page the layouts are made for (the main window's minimum size). */
const LAYOUT_MIN = { w: 960, h: 600 }
let wantedScale = 100
let watchingResize = false

/** The zoom for the chosen size, never more than keeps a LAYOUT_MIN page in the window (a bigger size in a small
 *  window grows only as far as the layouts still fit). */
function fitZoom(): number {
  const fit = Math.min(window.innerWidth / LAYOUT_MIN.w, window.innerHeight / LAYOUT_MIN.h)
  return Math.max(Math.min(1, wantedScale / 100), Math.min(wantedScale / 100, fit))
}

function setZoom() {
  const z = Math.round(fitZoom() * 1000) / 1000
  document.documentElement.style.setProperty('zoom', z === 1 ? '' : String(z))
}

/** Interface size and reduced motion on <html>: `zoom` scales the whole page (layout and text, every page and
 *  popup), `data-motion="reduce"` turns transitions and animations off (base.css). */
export function applyLayout(scale: number, reduceMotion: boolean) {
  const el = document.documentElement
  const s = UI_SCALES.includes(scale as (typeof UI_SCALES)[number]) ? scale : 100
  wantedScale = s
  setZoom()
  if (!watchingResize) {
    watchingResize = true
    window.addEventListener('resize', setZoom)
  }
  if (reduceMotion) el.dataset.motion = 'reduce'
  else delete el.dataset.motion
  try {
    localStorage.setItem(LAYOUT_KEY, JSON.stringify({ scale: s, reduceMotion }))
  } catch {
    /* fine: the saved settings re-apply it a moment after start */
  }
}

/** The page zoom set by applyLayout (1 at 100%). */
export function uiZoom(): number {
  return parseFloat(document.documentElement.style.getPropertyValue('zoom')) || 1
}

/** Synchronous, from localStorage: runs before React renders. */
export function initTheme() {
  let t = { accent: 'green', look: 'default' }
  let l = { scale: 100, reduceMotion: false }
  try {
    t = { ...t, ...JSON.parse(localStorage.getItem(KEY) ?? '{}') }
    l = { ...l, ...JSON.parse(localStorage.getItem(LAYOUT_KEY) ?? '{}') }
  } catch {
    /* defaults */
  }
  applyTheme(t.accent, t.look)
  applyLayout(l.scale, l.reduceMotion)
}

/** theme.css from the config dir, appended after tokens.css (same cascade, later wins). Called again to reload it
 *  after an edit (Settings › Appearance). */
export async function injectThemeOverride() {
  try {
    const css = await api.getThemeOverride()
    let style = document.getElementById('theme-override')
    if (!css.trim()) {
      style?.remove()
      syncWindowBg()
      return
    }
    if (!style) {
      style = document.createElement('style')
      style.id = 'theme-override'
      document.head.appendChild(style)
    }
    if (style.textContent !== css) style.textContent = css
    syncWindowBg()
  } catch {
    /* no override */
  }
}
