// Settings › Appearance: accent, look (previewed with the look's own tokens), size, motion, your own theme.css.
import { useEffect, useState } from 'react'
import { api } from '../../lib/ipc'
import { updateSettings, useSettings } from '../../lib/settings'
import { ACCENTS, LOOKS, UI_SCALES, injectThemeOverride } from '../../lib/theme'
import { toast } from '../../lib/ui'
import { Btn, Group, Match, Pane, Row, Seg, Switch } from './kit'

const name = (s: string) => s[0].toUpperCase() + s.slice(1)

function ThemeFile() {
  const [f, setF] = useState<{ path: string; exists: boolean } | null>(null)
  const load = () => api.themeFile().then(setF, () => setF(null))
  useEffect(() => {
    void load()
    // Back from the editor: pick up the saved file.
    const back = () => { void load(); void injectThemeOverride() }
    window.addEventListener('focus', back)
    return () => window.removeEventListener('focus', back)
  }, [])
  if (!f) return null
  const open = () => api.themeOpen().then(() => { void load(); void injectThemeOverride() }, (e) => toast(String(e)))
  return (
    <Group title="Your own theme">
      <Row label="theme.css" sub={f.path} mono keywords="css override tokens custom">
        <span className="sub">{f.exists ? 'In use' : 'Not created'}</span>
        <Btn onClick={() => void open()}>{f.exists ? 'Open' : 'Create and open'}</Btn>
      </Row>
    </Group>
  )
}

export function AppearancePane() {
  const s = useSettings((x) => x)
  return (
    <Pane title="Appearance">
      <Match label="Accent colour color">
        <Group title="Accent">
          <div className="set-row">
            <div className="set-sws">
              {ACCENTS.map((a) => (
                <button key={a} title={name(a)} aria-label={name(a)} className={s.accent === a ? 'on' : ''} style={{ background: `var(--swatch-${a})` }} onClick={() => void updateSettings({ accent: a })} />
              ))}
            </div>
          </div>
        </Group>
      </Match>
      <Match label="Look theme dark midnight dim">
        <Group title="Look">
          <div className="set-looks">
            {LOOKS.map((l) => (
              <button key={l} className={`set-look${s.look === l ? ' on' : ''}`} onClick={() => void updateSettings({ look: l })}>
                {/* The preview carries the look itself, so it shows that look's real tokens. */}
                <span className="pv" data-theme="dark" data-look={l === 'default' ? undefined : l}>
                  <i className="a" /><i className="b" /><i className="c" />
                </span>
                <span className="nm">{name(l)}</span>
              </button>
            ))}
          </div>
        </Group>
      </Match>
      <Group title="Layout">
        <Row label="Size" keywords="zoom scale interface bigger smaller">
          <Seg value={UI_SCALES.includes(s.ui_scale as (typeof UI_SCALES)[number]) ? s.ui_scale : 100} options={UI_SCALES.map((n) => [n, `${n}%`])} onChange={(v) => void updateSettings({ ui_scale: v })} />
        </Row>
        <Row label="Reduce motion" keywords="animation">
          <Switch on={s.reduce_motion} onChange={(v) => void updateSettings({ reduce_motion: v })} />
        </Row>
      </Group>
      <ThemeFile />
    </Pane>
  )
}
