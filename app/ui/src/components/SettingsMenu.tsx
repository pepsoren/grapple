// The avatar menu (mockups/08-settings.html, screen B): who you are, the two switches you flip often, then Settings.
// Everything else is on the Settings page.
import { api } from '../lib/ipc'
import { updateSettings, useSettings } from '../lib/settings'
import { openDownloads, openSettings, toast, uiStore, useUi } from '../lib/ui'
import { tether, useTether } from '../tether'
import { Avatar } from './Avatar'
import { DownloadIcon } from './Icons'
import { GearIcon, PlayingIcon, QuitIcon, ScanIcon, SocialIcon } from './settings/icons'

export function SettingsMenu() {
  const open = useUi((s) => s.menu)
  const s = useSettings((x) => x)
  const t = useTether((x) => x)
  if (!open) return null
  const status = !t.signedIn ? 'Not signed in to tether'
    : t.connection === 'online' ? 'Online on tether'
      : t.connection === 'reconnecting' ? 'Reconnecting to tether'
        : t.connection === 'error' ? 'tether: connection problem' : 'Connecting to tether'
  const close = () => uiStore.set({ menu: false })
  return (
    <>
      <div style={{ position: 'absolute', inset: 0, zIndex: 19 }} onClick={close} />
      <div className="menu" role="menu">
        <div className="who">
          <Avatar f={{ ...t.me, name: t.me.name }} size={40} noTitle />
          <div><b>{t.me.name || 'grapple'}</b><span className={t.signedIn && t.connection === 'online' ? 'ok' : ''}>{status}</span></div>
        </div>
        <hr />
        <div className="it">
          <SocialIcon />Show what I play
          <button className={`tog${s.show_in_tether ? ' on' : ''}`} role="switch" aria-checked={s.show_in_tether} aria-label="Show what I play" onClick={() => void updateSettings({ show_in_tether: !s.show_in_tether })} />
        </div>
        <div className="it">
          <PlayingIcon />Keep Steam ready
          <button className={`tog${s.keep_steam_ready ? ' on' : ''}`} role="switch" aria-checked={s.keep_steam_ready} aria-label="Keep Steam ready" onClick={() => void updateSettings({ keep_steam_ready: !s.keep_steam_ready })} />
        </div>
        <hr />
        <button className="it" role="menuitem" onClick={() => openSettings()}><GearIcon />Settings<span className="k"><kbd>Ctrl</kbd><kbd>,</kbd></span></button>
        <button className="it" role="menuitem" onClick={openDownloads}><DownloadIcon />Downloads</button>
        <button className="it" role="menuitem" onClick={() => { close(); void api.rescan().then(() => toast('Rescanning your libraries')) }}><ScanIcon />Rescan libraries</button>
        <hr />
        {t.signedIn
          ? <button className="it" role="menuitem" onClick={() => { close(); void tether.signOut() }}><SocialIcon />Sign out of tether</button>
          : <button className="it" role="menuitem" onClick={() => void tether.signIn()}><SocialIcon />{t.signingIn ? 'Waiting for the browser…' : 'Sign in with tether'}</button>}
        <button className="it q" role="menuitem" onClick={() => void api.quit()}><QuitIcon />Quit grapple</button>
      </div>
    </>
  )
}
