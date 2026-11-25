/** The grapple mark (brand/grapple-mark-mono.svg, currentColor). The mask colours are luminance tokens. */
export function BrandMark({ size = 24 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 100 100" role="img" aria-label="grapple" fill="currentColor">
      <defs>
        <clipPath id="grapple-tile"><rect width="100" height="100" rx="33.33" /></clipPath>
        <mask id="grapple-hook" maskUnits="userSpaceOnUse" x="-20" y="-20" width="140" height="140">
          <rect x="-20" y="-20" width="140" height="140" style={{ fill: 'var(--mask-show)' }} />
          <path d="M68.5 71.5 L26.07 113.93" strokeWidth="7.8" style={{ stroke: 'var(--mask-hide)' }} />
        </mask>
      </defs>
      <g clipPath="url(#grapple-tile)">
        <g mask="url(#grapple-hook)">
          <path d="M75.00 65.72 L71.24 18.00 L28.00 18.00 L21.00 67.00 L29.26 52.60 L41.75 49.91 L33.80 44.68 L41.65 31.00 L59.27 31.00 L62.00 65.72 Z" />
          <path d="M58.0 71.5 a10.5 10.5 0 1 0 21.0 0 a10.5 10.5 0 1 0 -21.0 0 Z M64.2 71.5 a4.3 4.3 0 1 1 8.6 0 a4.3 4.3 0 1 1 -8.6 0 Z" fillRule="evenodd" />
        </g>
        <path d="M67.86 72.14 L26.07 113.93" strokeWidth="4.2" fill="none" stroke="currentColor" />
      </g>
    </svg>
  )
}
