// A key hint that turns into the pad's button (Settings › Controllers › Button prompts) while a pad was used last.
import { usePadGlyph, type Glyph } from '../lib/pads'
import type { PadAction, PadLayout } from '../lib/types'

function Shape({ shape }: { shape: 'cross' | 'circle' | 'square' | 'triangle' }) {
  return (
    <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
      {shape === 'cross' && <path d="M2 2l6 6M8 2l-6 6" />}
      {shape === 'circle' && <circle cx="5" cy="5" r="3.4" />}
      {shape === 'square' && <rect x="1.8" y="1.8" width="6.4" height="6.4" rx="0.6" />}
      {shape === 'triangle' && <path d="M5 1.6l3.6 6.4H1.4z" />}
    </svg>
  )
}

const NAMES = { cross: 'Cross', circle: 'Circle', square: 'Square', triangle: 'Triangle' } as const

/** One pad button as a small chip: a round face button, or a word for shoulders and menus. */
export function GlyphChip({ g, style }: { g: Glyph; style: PadLayout }) {
  if (g.kind === 'word') return <span className="pglyph word" data-style={style}>{g.text}</span>
  if (g.kind === 'shape') return <span className="pglyph face" data-style={style} aria-label={NAMES[g.shape]}><Shape shape={g.shape} /></span>
  return <span className="pglyph face" data-style={style}>{g.text}</span>
}

/** `keyText` in a .kbd chip, or the pad's button for `action` when a pad was used last. */
export function Hint({ action, keyText }: { action: PadAction; keyText: string }) {
  const pad = usePadGlyph(action)
  if (!pad) return <span className="kbd">{keyText}</span>
  return <GlyphChip g={pad.glyph} style={pad.style} />
}
