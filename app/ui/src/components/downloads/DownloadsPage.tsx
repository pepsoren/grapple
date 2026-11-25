// A · Downloads (mockups/07-downloads.html): the active download, Up next, Updates, Done. Renders what Rust pushes.
import { useEffect, useState } from 'react'
import type { DlItem } from '../../lib/download-types'
import {
  artOf, cancelItem, left, loadDownloadSettings, ofTotal, pauseAll, pauseItem, progress, refreshDownloads, reorder, resumeItem,
  since, size, speed, startNow, STATE_WORD, updateDownloadSettings, updateNow, useDownloads,
} from '../../lib/downloads'
import { play } from '../../lib/session'
import { CloseIcon, PauseIcon, PlayIcon } from '../Icons'
import { Bar, Spark, StoreBadge } from './parts'
import '../../styles/downloads.css'

function Now({ item }: { item: DlItem }) {
  const paused = item.state === 'paused'
  return (
    <div className="dl-now">
      {artOf(item) && <img src={item.art.hero ?? artOf(item)} alt="" />}
      <div className="sh" />
      <div className="in">
        <div>
          <div className="k"><StoreBadge store={item.store} />{item.state === 'error' ? <span className="err">{item.error}</span> : STATE_WORD[item.state]}</div>
          <div className="t">{item.name}</div>
        </div>
        <div className="ctl">
          {item.canPause && <button className="dl-btn dl-ghost dl-icon" title="Pause" onClick={() => void pauseItem(item.game)}><PauseIcon /></button>}
          {paused && item.store === 'epic' && <button className="dl-btn dl-ghost dl-icon" title="Resume" onClick={() => void resumeItem(item.game)}><PlayIcon /></button>}
          {item.canCancel && <button className="dl-btn dl-ghost dl-icon" title="Cancel" onClick={() => void cancelItem(item.game)}><CloseIcon size={18} /></button>}
        </div>
        <div className="dl-stats">
          <span><b>{ofTotal(item.bytesDone, item.bytesTotal)}</b>downloaded</span>
          <span><b>{speed(item.bytesPerSec)}</b>speed</span>
          <span><b>{left(item.secondsLeft)}</b>left</span>
          <span><b>{item.targetDir}</b>drive</span>
        </div>
        <Spark history={item.speedHistory} />
        <Bar value={progress(item)} />
      </div>
    </div>
  )
}

function Row({ item, update, bridge, drag }: {
  item: DlItem; update?: boolean; bridge: boolean
  drag: { id: string | null; start: (id: string) => void; over: (id: string) => void; end: () => void }
}) {
  const word = item.state === 'error' ? <span className="err">{item.error}</span> : item.state !== 'queued' ? STATE_WORD[item.state] : null
  const sizeText = item.state === 'downloading' ? ofTotal(item.bytesDone, item.bytesTotal) : size(item.bytesTotal)
  // Steam rows can only be started with the bridge (Steam's own order otherwise).
  const canStart = item.store === 'epic' || bridge
  return (
    <div
      className={`dl-qr${drag.id === item.game ? ' dragging' : ''}`}
      draggable={item.canReorder}
      onDragStart={(e) => { e.dataTransfer.effectAllowed = 'move'; drag.start(item.game) }}
      onDragOver={(e) => { if (drag.id) { e.preventDefault(); drag.over(item.game) } }}
      onDragEnd={drag.end}
      onDrop={(e) => { e.preventDefault(); drag.end() }}
    >
      <span className="grip">{item.canReorder ? '⋮⋮' : ''}</span>
      {artOf(item) ? <img className="art" src={artOf(item)} alt="" /> : <span className="art" />}
      <div className="dl-qn">
        <b>{item.name}</b>
        <span>
          <StoreBadge store={item.store} />
          {item.store === 'epic' && !update && ' downloads in grapple'}
          {update && ' update'}
          {word && <> · {word}</>}
        </span>
      </div>
      <span className="sub">{sizeText}</span>
      <span className="sub">{item.targetDir}</span>
      {canStart && item.state !== 'downloading'
        ? <button className="dl-btn dl-ghost dl-sm" onClick={() => void (update ? updateNow(item.game) : startNow(item.game))}>{update ? 'Update now' : 'Start now'}</button>
        : <span />}
      {item.canCancel
        ? <button className="dl-btn dl-ghost dl-icon dl-sm" title="Cancel" onClick={() => void cancelItem(item.game)}><CloseIcon size={16} /></button>
        : <span />}
    </div>
  )
}

/** Drag to reorder: Epic among Epic, Steam among Steam (only with the bridge: `canReorder`). */
function useDrag(list: DlItem[]) {
  const [id, setId] = useState<string | null>(null)
  const [order, setOrder] = useState<string[] | null>(null)
  const ids = order ?? list.map((i) => i.game)
  const by = new Map(list.map((i) => [i.game, i]))
  const shown = ids.map((g) => by.get(g)).filter((x): x is DlItem => !!x)
  return {
    shown,
    drag: {
      id,
      start: (g: string) => { setId(g); setOrder(list.map((i) => i.game)) },
      over: (g: string) => {
        if (!id || g === id || !order) return
        const a = by.get(id), b = by.get(g)
        if (!a || !b || a.store !== b.store || !b.canReorder) return
        const next = order.filter((x) => x !== id)
        next.splice(next.indexOf(g) + (order.indexOf(g) > order.indexOf(id) ? 1 : 0), 0, id)
        setOrder(next)
      },
      end: () => {
        const store = id ? by.get(id)?.store : undefined
        if (order && store) void reorder(order.filter((g) => by.get(g)?.store === store))
        setId(null)
        setOrder(null)
      },
    },
  }
}

export default function DownloadsPage() {
  const snap = useDownloads((s) => s.snap)
  const settings = useDownloads((s) => s.settings)
  useEffect(() => { void refreshDownloads(); void loadDownloadSettings() }, [])
  const queued = snap?.queued ?? []
  const updates = snap?.updates ?? []
  const done = snap?.done ?? []
  const q = useDrag(queued)
  const u = useDrag(updates)
  const bridge = snap?.steamBridge === 'on'
  const nothing = !snap?.active && !queued.length && !updates.length && !done.length
  const totalQueued = queued.reduce((a, i) => a + (i.bytesTotal ?? 0), 0)
  return (
    <div className="app dl-page">
      <div className="dl-ph">
        <h2>Downloads</h2>
        {!nothing && (
          <div className="r"><button className="dl-btn dl-ghost dl-sm" onClick={() => void pauseAll(!snap?.pausedAll)}>{snap?.pausedAll ? 'Resume all' : 'Pause all'}</button></div>
        )}
      </div>
      {nothing && <div className="dl-empty">Nothing downloading</div>}
      {snap?.active && <Now item={snap.active} />}
      {queued.length > 0 && (
        <>
          <div className="dl-h3">Up next<span className="r">{queued.length} {queued.length === 1 ? 'game' : 'games'} · {size(totalQueued)}</span></div>
          <div className="dl-q">{q.shown.map((i) => <Row key={i.game} item={i} bridge={bridge} drag={q.drag} />)}</div>
        </>
      )}
      {updates.length > 0 && (
        <>
          <div className="dl-h3">
            Updates
            {settings && (
              <span className="r dl-tg">
                Only while I'm not playing
                <button className={`tog${settings.updateOnlyWhenIdle ? ' on' : ''}`} role="switch" aria-checked={settings.updateOnlyWhenIdle} onClick={() => void updateDownloadSettings({ updateOnlyWhenIdle: !settings.updateOnlyWhenIdle })} />
              </span>
            )}
          </div>
          <div className="dl-q">{u.shown.map((i) => <Row key={i.game} item={i} update bridge={bridge} drag={u.drag} />)}</div>
        </>
      )}
      {done.length > 0 && (
        <>
          <div className="dl-h3">Done</div>
          <div className="dl-q done">
            {done.map((i) => (
              <div className="dl-qr" key={i.game}>
                {artOf(i) ? <img className="art" src={artOf(i)} alt="" /> : <span className="art" />}
                <div className="dl-qn"><b>{i.name}</b><span>{i.kind === 'update' ? 'Updated' : 'Installed'} {since(i.finishedAt)}</span></div>
                <button className="dl-btn dl-play dl-sm" onClick={() => play(i.game)}>▶ Play</button>
              </div>
            ))}
          </div>
        </>
      )}
    </div>
  )
}
