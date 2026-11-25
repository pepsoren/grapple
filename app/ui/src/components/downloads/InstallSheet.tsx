// C · Install (mockups/07-downloads.html): art, the drives with their free space, the download size, "Runs with" for
// Epic on Linux, Cancel / Install. Opened by every Install button (lib/library.ts installGame).
import { useEffect, useState } from 'react'
import type { InstallTarget } from '../../lib/download-types'
import { closeSheet, confirmInstall, size, useDownloads } from '../../lib/downloads'
import { artUrl } from '../../lib/ipc'
import { Bar, Pick } from './parts'
import '../../styles/downloads.css'

function Drive({ t, on, pick }: { t: InstallTarget; on: boolean; pick: () => void }) {
  const used = t.totalBytes ? 1 - t.freeBytes / t.totalBytes : 0
  return (
    <button className={`dl-drive${on ? ' on' : ''}${t.enough ? '' : ' no'}`} disabled={!t.enough} onClick={pick} role="radio" aria-checked={on}>
      <span className="dl-radio" />
      <div><b>{t.label}</b><span className="path">{t.path}</span></div>
      <div className="dl-space">
        {t.enough ? `${size(t.freeBytes)} free` : <span className="warn">{size(t.freeBytes)} free</span>}
        <Bar value={used} warn={!t.enough} />
      </div>
    </button>
  )
}

export function InstallSheet() {
  const sheet = useDownloads((s) => s.sheet)
  const [target, setTarget] = useState<string | null>(null)
  const [proton, setProton] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  const plan = sheet?.plan ?? null
  useEffect(() => {
    if (!plan) { setTarget(null); setProton(null); return }
    const ok = plan.targets.filter((t) => t.enough)
    setTarget((ok.find((t) => t.isDefault) ?? ok[0])?.path ?? null)
    setProton(plan.protons?.find((p) => p.isDefault)?.name ?? plan.protons?.[0]?.name ?? null)
  }, [plan])
  useEffect(() => {
    if (!sheet) return
    const key = (e: KeyboardEvent) => { if (e.key === 'Escape') { e.stopPropagation(); closeSheet() } }
    document.addEventListener('keydown', key, true)
    return () => document.removeEventListener('keydown', key, true)
  }, [sheet])
  if (!sheet) return null
  const g = sheet.game
  const art = g.art.hero ? artUrl(g, 'hero') : g.art.cover ? artUrl(g, 'cover') : null
  const go = async () => {
    if (!target || busy) return
    setBusy(true)
    await confirmInstall(g, target, plan?.protons ? proton : null)
    setBusy(false)
  }
  return (
    <div className="dl-scrim" onClick={closeSheet}>
      <div className="dl-sheet" role="dialog" aria-modal onClick={(e) => e.stopPropagation()}>
        <div className="art">{art && <img src={art} alt="" />}</div>
        <div className="b">
          <h3>Install {g.name}</h3>
          {sheet.error && <div className="err">{sheet.error}</div>}
          {plan?.targets.map((t) => <Drive key={t.path} t={t} on={t.path === target} pick={() => t.enough && setTarget(t.path)} />)}
          {(plan?.sizeBytes != null || !plan) && !sheet.error && (
            <div className="dl-row2"><span>Download size</span><span className="sub">{plan ? size(plan.sizeBytes) : 'Checking size'}</span></div>
          )}
          {plan?.protons && plan.protons.length > 0 && (
            <div className="dl-row2">
              <span>Runs with</span>
              <Pick label={proton ?? 'Heroic default'}>
                {(close) => plan.protons!.map((p) => (
                  <button key={p.name} className={p.name === proton ? 'on' : ''} onClick={() => { setProton(p.name); close() }}>{p.name}{p.name === proton && <i>✓</i>}</button>
                ))}
              </Pick>
            </div>
          )}
          <div className="dl-acts">
            <button className="dl-btn dl-sec" onClick={closeSheet}>Cancel</button>
            <button className="dl-btn dl-play" disabled={!target || busy} onClick={() => void go()}>Install</button>
          </div>
        </div>
      </div>
    </div>
  )
}
