import type { Game } from './types'

/** "16h 16m", "4h 10m", "262h" (round hours from 100h), "" for 0. Mirrors the mockup's `hrsLong`/`hrs`. */
export function hoursLong(min: number): string {
  if (!min) return 'Never played'
  return `${Math.floor(min / 60)}h ${String(min % 60).padStart(2, '0')}m`
}

/** Short form for lists: "45m", "16.3h", "262h". */
export function hoursShort(min: number): string {
  if (!min) return ''
  if (min < 60) return `${min}m`
  if (min < 6000) return `${(Math.round(min / 6) / 10).toString().replace(/\.0$/, '')}h`
  return `${Math.round(min / 60)}h`
}

/** "4h 45m" for stats; "-" for zero. */
export function hm(min: number): string {
  if (!min) return '—'
  const h = Math.floor(min / 60)
  const m = min % 60
  return h ? `${h}h ${String(m).padStart(2, '0')}m` : `${m}m`
}

const DAY = 86400
export const nowSec = () => Math.floor(Date.now() / 1000)

/** "Today", "Yesterday", "Tuesday", "Oct 5", "Never". */
export function ago(t: number | null | undefined): string {
  if (!t) return 'Never'
  const d = Math.floor((nowSec() - t) / DAY)
  if (d < 1) return 'Today'
  if (d < 2) return 'Yesterday'
  const date = new Date(t * 1000)
  if (d < 7) return date.toLocaleDateString('en-GB', { weekday: 'long' })
  return date.toLocaleDateString('en-GB', { day: 'numeric', month: 'short' })
}

/** The Recent timeline buckets of the Collection overview. */
export function bucket(t: number): string {
  const d = (nowSec() - t) / DAY
  if (d < 7) return 'This week'
  if (d < 14) return 'Last week'
  if (d < 31) return '2 weeks ago'
  return new Date(t * 1000).toLocaleDateString('en-GB', { month: 'long' })
}

export function clock(totalSec: number): string {
  const s = Math.max(0, totalSec)
  const h = Math.floor(s / 3600)
  const m = Math.floor((s % 3600) / 60)
  const x = s % 60
  return `${h}:${String(m).padStart(2, '0')}:${String(x).padStart(2, '0')}`
}

export function storeLabel(g: Game): string {
  return g.store === 'epic' ? 'Epic' : g.store === 'steam' ? 'Steam' : 'Other'
}

export function launchSteps(g: Game): string[] {
  return g.store === 'epic'
    ? ['Epic sign-in token', 'Proton ready', 'Starting game']
    : ['Fetching game from Steam', 'Game files ready', 'Starting game']
}

/** Subsequence match with highlight; ranks prefix and consecutive hits first. */
export function fuzzy(name: string, q: string): { ok: boolean; score: number; marks: number[] } {
  if (!q) return { ok: true, score: 0, marks: [] }
  const n = name.toLowerCase()
  const lq = q.toLowerCase()
  // Prefer a plain substring (best), then a word-start subsequence, then any subsequence.
  const at = n.indexOf(lq)
  if (at >= 0) {
    const marks = Array.from({ length: lq.length }, (_, i) => at + i)
    return { ok: true, score: 1000 - at + (at === 0 ? 200 : 0) - n.length, marks }
  }
  const marks: number[] = []
  let j = 0
  let score = 0
  let prev = -2
  for (let i = 0; i < n.length && j < lq.length; i++) {
    if (n[i] === lq[j]) {
      marks.push(i)
      score += (i === prev + 1 ? 5 : 1) + (i === 0 || n[i - 1] === ' ' ? 4 : 0)
      prev = i
      j++
    }
  }
  return { ok: j === lq.length, score: score - n.length / 10, marks }
}
