// The Settings page (mockups/08-settings.html, screen A): the sections on the left with "Find a setting" and the
// version, the section on the right. While searching, every section's matching rows show at once.
import { useEffect, useRef, useState, type ComponentType } from 'react'
import { usePlatform } from '../../lib/platform'
import { closeSettings, uiStore, useUi, type SettingsSection } from '../../lib/ui'
import { DownloadSettingsPane } from '../downloads/DownloadSettings'
import { StoreSettingsPane } from '../store/StoreSettingsPage'
import { AccountsPane } from './Accounts'
import { AdvancedPane } from './Advanced'
import { AppearancePane } from './Appearance'
import { ControllersPane } from './Controllers'
import { GeneralPane } from './General'
import {
  AccountIcon, AdvancedIcon, AppearanceIcon, BellIcon, DownloadsIcon, FindIcon, GeneralIcon, KeysIcon, LibraryIcon, PadIcon, PlayingIcon,
  SocialIcon, StoreIcon,
} from './icons'
import { FindQuery, Match } from './kit'
import { LibraryPane } from './Library'
import { NotificationsPane } from './Notifications'
import { PlayingPane } from './Playing'
import { ShortcutsPane } from './Shortcuts'
import { SocialPane } from './Social'
import '../../styles/downloads.css'
import '../../styles/store-page.css'
import '../../styles/settings.css'

/** Settings › Controllers is the controllers' own pane (title included); while searching it shows when its name
 *  matches. */
function Controllers() {
  return (
    <Match label="Controllers controller gamepad">
      <div className="set-own"><ControllersPane /></div>
    </Match>
  )
}

const SECTIONS: [SettingsSection, string, ComponentType, ComponentType][] = [
  ['general', 'General', GeneralIcon, GeneralPane],
  ['accounts', 'Accounts', AccountIcon, AccountsPane],
  ['appearance', 'Appearance', AppearanceIcon, AppearancePane],
  ['library', 'Library', LibraryIcon, LibraryPane],
  ['playing', 'Playing', PlayingIcon, PlayingPane],
  ['controllers', 'Controllers', PadIcon, Controllers],
  ['downloads', 'Downloads', DownloadsIcon, DownloadSettingsPane],
  ['store', 'Store', StoreIcon, StoreSettingsPane],
  ['social', 'Friends & tether', SocialIcon, SocialPane],
  ['notifications', 'Notifications', BellIcon, NotificationsPane],
  ['shortcuts', 'Shortcuts', KeysIcon, ShortcutsPane],
  ['advanced', 'Advanced', AdvancedIcon, AdvancedPane],
]

export default function SettingsPage() {
  const section = useUi((s) => s.settings)
  const version = usePlatform()?.version
  // Dev screenshots: ?find=<text> opens the page searching.
  const [q, setQ] = useState(() => (import.meta.env.DEV ? new URLSearchParams(location.search).get('find') ?? '' : ''))
  const main = useRef<HTMLDivElement>(null)
  const query = q.trim().toLowerCase()
  useEffect(() => { main.current?.scrollTo({ top: 0 }) }, [section, query])
  const Current = SECTIONS.find(([k]) => k === section)?.[3] ?? GeneralPane
  return (
    <div className="set-page">
      <nav className="set-side" aria-label="Settings sections">
        <h2>Settings</h2>
        <label className="set-find">
          <FindIcon />
          <input
            value={q}
            placeholder="Find a setting"
            spellCheck={false}
            onChange={(e) => setQ(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Escape' && q) { e.stopPropagation(); setQ('') }
              else if (e.key === 'Escape') { (e.target as HTMLInputElement).blur(); closeSettings() }
            }}
          />
        </label>
        {SECTIONS.map(([k, label, Icon]) => (
          <button key={k} className={`set-si${!query && k === section ? ' on' : ''}`} onClick={() => { setQ(''); uiStore.set({ settings: k }) }}>
            <Icon />{label}
          </button>
        ))}
        {version && <div className="set-ver">grapple {version}</div>}
      </nav>
      <div className={`set-main${query ? ' finding' : ''}`} ref={main}>
        <FindQuery.Provider value={query}>
          {query ? SECTIONS.map(([k, , , P]) => <P key={k} />) : <Current />}
          {query && <div className="set-none">Nothing matches “{q.trim()}”</div>}
        </FindQuery.Provider>
      </div>
    </div>
  )
}
