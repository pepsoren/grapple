// Settings › Shortcuts: the keys grapple really has (App.tsx, Collection.tsx, Palette.tsx key handlers).
import type { ReactNode } from 'react'
import { Group, Match, Pane } from './kit'

const K = ({ children }: { children: ReactNode }) => <kbd>{children}</kbd>

function Keys({ rows }: { rows: [string, ReactNode][] }) {
  return (
    <>
      {rows.map(([label, keys]) => (
        <Match key={label} label={`${label} shortcut keys`}>
          <div className="set-key"><span>{label}</span><span>{keys}</span></div>
        </Match>
      ))}
    </>
  )
}

export function ShortcutsPane() {
  return (
    <Pane title="Shortcuts">
      <Group title="Everywhere">
        <Keys rows={[
          ['Back, close', <K>Esc</K>],
          ['Home · Library · Collection · Store', <><K>Ctrl</K><K>1</K>–<K>4</K></>],
          ['Settings', <><K>Ctrl</K><K>,</K></>],
        ]} />
      </Group>
      <Group title="Home">
        <Keys rows={[
          ['Find a game', <><K>/</K> or just start typing</>],
          ['Next / previous game', <><K>←</K><K>→</K></>],
          ['Play', <K>Enter</K>],
          ['Details', <K>Tab</K>],
        ]} />
      </Group>
      <Group title="Collection">
        <Keys rows={[
          ['Search', 'Just start typing'],
          ['Next / previous game', <><K>↑</K><K>↓</K></>],
          ['Play', <K>Enter</K>],
        ]} />
      </Group>
      <Group title="Find a game">
        <Keys rows={[
          ['Next / previous result', <><K>↑</K><K>↓</K></>],
          ['Play', <K>Enter</K>],
          ['Details', <K>Tab</K>],
        ]} />
      </Group>
    </Pane>
  )
}
