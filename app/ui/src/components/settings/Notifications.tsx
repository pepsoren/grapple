// Settings › Notifications: the desktop notifications grapple sends (a download is ready, a wishlist sale) and
// holding them while a game runs.
import { useEffect } from 'react'
import { loadDownloadSettings, updateDownloadSettings, useDownloads } from '../../lib/downloads'
import { useLinuxOrWindows } from '../../lib/platform'
import { updateSettings, useSettings } from '../../lib/settings'
import { startStore, updateStoreSettings, useStoreData } from '../../lib/store-data'
import { Group, Pane, Row, Switch } from './kit'

export function NotificationsPane() {
  const quiet = useSettings((s) => s.quiet_while_playing)
  const dl = useDownloads((x) => x.settings)
  const store = useStoreData((x) => x.settings)
  // notify-send on Linux desktops, toasts on Windows. macOS gets none yet, so there is nothing to hold.
  const linux = useLinuxOrWindows()
  useEffect(() => {
    void loadDownloadSettings()
    startStore()
  }, [])
  return (
    <Pane title="Notifications">
      {(dl || store) && (
        <Group title="Tell me when">
          {dl && (
            <Row label="A game is ready to play" keywords="download installed">
              <Switch on={dl.notifyReady} onChange={(v) => void updateDownloadSettings({ notifyReady: v })} />
            </Row>
          )}
          {store && (
            <Row label="A wishlist game goes on sale" keywords="store">
              <Switch on={store.wishlistAlerts} onChange={(v) => void updateStoreSettings({ wishlistAlerts: v })} />
            </Row>
          )}
        </Group>
      )}
      {linux && (
        <Group title="Quiet">
          <Row label="Nothing while I'm playing" sub="Sent when you're done" keywords="do not disturb">
            <Switch on={quiet} onChange={(v) => void updateSettings({ quiet_while_playing: v })} />
          </Row>
        </Group>
      )}
    </Pane>
  )
}
