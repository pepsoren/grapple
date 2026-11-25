import type { SVGProps } from 'react'

type P = { size?: number } & SVGProps<SVGSVGElement>
const base = (size = 18) => ({ width: size, height: size, viewBox: '0 0 24 24' })

export const PlayIcon = ({ size = 18, ...p }: P) => (
  <svg {...base(size)} fill="currentColor" {...p}><path d="M7 4.5v15l13-7.5z" /></svg>
)
export const DotsIcon = ({ size = 20, ...p }: P) => (
  <svg {...base(size)} fill="currentColor" {...p}><circle cx="5" cy="12" r="2" /><circle cx="12" cy="12" r="2" /><circle cx="19" cy="12" r="2" /></svg>
)
export const SearchIcon = ({ size = 16, ...p }: P) => (
  <svg {...base(size)} fill="none" stroke="currentColor" strokeWidth="2.4" {...p}><circle cx="11" cy="11" r="7" /><path d="m20 20-4-4" /></svg>
)
export const PlusIcon = ({ size = 22, ...p }: P) => (
  <svg {...base(size)} fill="none" stroke="currentColor" strokeWidth="2.4" {...p}><path d="M12 5v14M5 12h14" /></svg>
)
export const CloseIcon = ({ size = 16, ...p }: P) => (
  <svg {...base(size)} fill="none" stroke="currentColor" strokeWidth="2.4" strokeLinecap="round" {...p}><path d="M6 6l12 12M18 6 6 18" /></svg>
)
export const HomeIcon = ({ size = 18, ...p }: P) => (
  <svg {...base(size)} fill="currentColor" {...p}><path d="M12 3 3 10v10a1 1 0 0 0 1 1h5v-6h6v6h5a1 1 0 0 0 1-1V10z" /></svg>
)
export const DownloadIcon = ({ size = 18, ...p }: P) => (
  <svg {...base(size)} fill="none" stroke="currentColor" strokeWidth="2.4" strokeLinecap="round" {...p}><path d="M12 4v11m-5-5 5 5 5-5M5 20h14" /></svg>
)
/** tether's real one-colour mark (assets/tether/tether-mark-mono.svg), drawn as a CSS mask so it takes the
 *  current text colour and any size. Never redraw it. */
export const TetherIcon = ({ size = 14, className = '' }: { size?: number; className?: string }) => (
  <span className={`tmark ${className}`} style={{ width: size, height: size }} role="img" aria-label="tether" />
)
export const StarIcon = ({ size = 18, on, ...p }: P & { on?: boolean }) => (
  <svg {...base(size)} fill={on ? 'currentColor' : 'none'} stroke="currentColor" strokeWidth="2" strokeLinejoin="round" {...p}>
    <path d="m12 3 2.7 5.6 6.1.8-4.5 4.2 1.1 6-5.4-3-5.4 3 1.1-6L3.2 9.4l6.1-.8z" />
  </svg>
)
export const PauseIcon = ({ size = 18, ...p }: P) => (
  <svg {...base(size)} fill="none" stroke="currentColor" strokeWidth="3" strokeLinecap="round" {...p}><path d="M8 5v14M16 5v14" /></svg>
)
/** A person and a plus: Invite (mockups/09-invites.html). */
export const InviteIcon = ({ size = 18, ...p }: P) => (
  <svg {...base(size)} fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" {...p}>
    <circle cx="9" cy="8" r="3.5" /><path d="M2.5 20c0-3.6 2.9-6 6.5-6s6.5 2.4 6.5 6" /><path d="M19 8v6M16 11h6" />
  </svg>
)
export const CheckIcon = ({ size = 12, ...p }: P) => (
  <svg {...base(size)} fill="none" stroke="currentColor" strokeWidth="3" strokeLinecap="round" strokeLinejoin="round" {...p}><path d="M6 12.5l4 4 8-9" /></svg>
)
