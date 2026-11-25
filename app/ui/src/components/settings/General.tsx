// Settings › General: startup, the window, Steam.
import { useLinuxOrWindows } from '../../lib/platform'
import { updateSettings, useSettings } from '../../lib/settings'
import { Choice, Group, Pane, Row, Switch } from './kit'

export function GeneralPane() {
  const s = useSettings((x) => x)
  // Start at login: an XDG autostart entry on Linux desktops, the Run key on Windows.
  const linux = useLinuxOrWindows()
  return (
    <Pane title="General">
      <Group title="Startup">
        {linux && (
          <Row label="Start grapple when I log in" keywords="autostart login">
            <Switch on={s.autostart} onChange={(v) => void updateSettings({ autostart: v })} />
          </Row>
        )}
        {linux && s.autostart && (
          <Row label="Start hidden in the tray" sub1>
            <Switch on={s.start_hidden} onChange={(v) => void updateSettings({ start_hidden: v })} />
          </Row>
        )}
        <Row label="Open on" keywords="start page">
          <Choice
            value={s.open_on}
            options={[['home', 'Home'], ['library', 'Library'], ['collection', 'Collection'], ['store', 'Store']]}
            onChange={(v) => void updateSettings({ open_on: v })}
          />
        </Row>
      </Group>
      <Group title="Window">
        <Row label="Closing the window" keywords="tray quit">
          <Choice
            value={s.close_to_tray}
            options={[[true, 'Keeps grapple in the tray'], [false, 'Quits grapple']]}
            onChange={(v) => void updateSettings({ close_to_tray: v })}
          />
        </Row>
        <Row label="When a game starts" keywords="hide card">
          <Choice
            value={s.on_game_start}
            options={[['card', 'Shrink to a card'], ['hide', 'Hide grapple'], ['keep', 'Keep it open']]}
            onChange={(v) => void updateSettings({ on_game_start: v })}
          />
        </Row>
        {s.on_game_start !== 'keep' && (
          <Row label="When the game closes">
            <Choice
              value={s.on_game_exit}
              options={[['show', 'Bring grapple back'], ['stay', 'Stay hidden']]}
              onChange={(v) => void updateSettings({ on_game_exit: v })}
            />
          </Row>
        )}
      </Group>
      <Group title="Steam">
        <Row label="Keep Steam ready" sub="Faster Steam launches">
          <Switch on={s.keep_steam_ready} onChange={(v) => void updateSettings({ keep_steam_ready: v })} />
        </Row>
      </Group>
    </Pane>
  )
}
