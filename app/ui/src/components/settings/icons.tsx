// The Settings page's line icons (mockups/08-settings.html symbols), drawn in currentColor.
import type { ReactNode } from 'react'

const I = ({ size = 18, children }: { size?: number; children: ReactNode }) => (
  <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
    {children}
  </svg>
)

export const GeneralIcon = () => <I><path d="M4 7h10M18 7h2M4 17h4M12 17h8" /><circle cx="16" cy="7" r="2.2" /><circle cx="10" cy="17" r="2.2" /></I>
export const AccountIcon = () => <I><circle cx="12" cy="8.5" r="3.6" /><path d="M5 20c1.2-3.6 4-5.2 7-5.2s5.8 1.6 7 5.2" /></I>
export const AppearanceIcon = () => (
  <I>
    <path d="M12 3.5a8.5 8.5 0 1 0 0 17c1.3 0 1.8-.8 1.8-1.7 0-1.3-1.1-1.6-1.1-2.8 0-1 .8-1.6 1.8-1.6h2.1c2.2 0 3.9-1.6 3.9-4.1C20.5 6.6 16.8 3.5 12 3.5Z" />
    <circle cx="7.8" cy="11" r="1.3" fill="currentColor" stroke="none" /><circle cx="10.5" cy="7.3" r="1.3" fill="currentColor" stroke="none" /><circle cx="15" cy="7.6" r="1.3" fill="currentColor" stroke="none" />
  </I>
)
export const LibraryIcon = () => <I><rect x="4" y="4" width="6" height="16" rx="1.6" /><path d="M14 5.5l4.2-1 3 14.8-4.2 1Z" /></I>
export const PlayingIcon = () => <I><path d="M7 5.5v13l11-6.5Z" /></I>
export const PadIcon = () => (
  <I>
    <path d="M7.5 7.5h9a4.5 4.5 0 0 1 4.4 3.6l.9 4.3a2.4 2.4 0 0 1-4.1 2.1L15.6 15.5H8.4l-2.1 2a2.4 2.4 0 0 1-4.1-2.1l.9-4.3a4.5 4.5 0 0 1 4.4-3.6Z" />
    <path d="M8 10.2v3M6.5 11.7h3" /><circle cx="15.5" cy="10.8" r="1" fill="currentColor" stroke="none" /><circle cx="17.8" cy="12.8" r="1" fill="currentColor" stroke="none" />
  </I>
)
export const DownloadsIcon = () => <I><path d="M12 4v11m-5-5 5 5 5-5M5 20h14" /></I>
export const StoreIcon = () => <I><path d="M5 9h14l-1 11H6Z M9 9V7a3 3 0 0 1 6 0v2" /></I>
export const SocialIcon = () => <I><circle cx="9" cy="9" r="3.2" /><path d="M3 19c.9-3 3.2-4.4 6-4.4s5.1 1.4 6 4.4M15.5 5.6a3.2 3.2 0 0 1 0 6.4M18 14.8c1.5.6 2.5 2 3 4.2" /></I>
export const BellIcon = () => <I><path d="M6 16V11a6 6 0 0 1 12 0v5l1.5 2h-15Z M10 20.5a2.2 2.2 0 0 0 4 0" /></I>
export const KeysIcon = () => <I><rect x="3" y="6" width="18" height="12" rx="2.5" /><path d="M7 10h.01M11 10h.01M15 10h.01M8 14h8" /></I>
export const AdvancedIcon = () => <I><path d="M5 7h14M5 12h14M5 17h9" /></I>
export const GearIcon = () => <I><circle cx="12" cy="12" r="3" /><path d="M12 2.8v2.6M12 18.6v2.6M21.2 12h-2.6M5.4 12H2.8M18.5 5.5l-1.8 1.8M7.3 16.7l-1.8 1.8M18.5 18.5l-1.8-1.8M7.3 7.3 5.5 5.5" /></I>
export const QuitIcon = () => <I><path d="M12 3.5v8M7.2 6.6a7 7 0 1 0 9.6 0" /></I>
export const ScanIcon = () => <I><path d="M20 12a8 8 0 1 1-2.3-5.7M20 4v4h-4" /></I>
export const FolderIcon = ({ size = 22 }: { size?: number }) => <I size={size}><path d="M3.5 7a2 2 0 0 1 2-2h4l2 2h7a2 2 0 0 1 2 2v8.5a2 2 0 0 1-2 2h-13a2 2 0 0 1-2-2Z" /></I>
export const FindIcon = () => <I size={16}><circle cx="11" cy="11" r="7" /><path d="m20 20-4-4" /></I>

/** Steam's and Epic's marks are drawn with the theme's text colours (no brand colours in grapple). */
export const SteamMark = ({ size = 26 }: { size?: number }) => (
  <svg width={size} height={size} viewBox="0 0 24 24" aria-hidden>
    <circle cx="12" cy="12" r="10" fill="var(--store-steam)" />
    <g fill="none" stroke="var(--surface-well)" strokeWidth="1.6">
      <circle cx="15.2" cy="9.4" r="3.1" /><circle cx="8.6" cy="15.6" r="2.2" strokeWidth="1.5" /><path d="M2.6 13.2 8 15.4m4.8-3.6-3 2.4" />
    </g>
    <circle cx="15.2" cy="9.4" r="1.4" fill="var(--surface-well)" />
  </svg>
)
export const EpicMark = ({ size = 24 }: { size?: number }) => (
  <svg width={size} height={size} viewBox="0 0 24 24" aria-hidden>
    <path d="M5 2.5h14v15.5L12 21.5 5 18Z" fill="var(--text)" />
    <path d="M8 6h4M8 6v8h4M8 10h3.5" stroke="var(--panel)" strokeWidth="1.5" fill="none" />
  </svg>
)
