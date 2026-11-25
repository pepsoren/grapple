// A 40-line store: no dependency, selectors via useSyncExternalStore.
import { useSyncExternalStore } from 'react'

export interface Store<T> {
  get: () => T
  set: (patch: Partial<T> | ((s: T) => Partial<T>)) => void
  subscribe: (fn: () => void) => () => void
}

export function createStore<T extends object>(initial: T): Store<T> {
  let state = initial
  const subs = new Set<() => void>()
  return {
    get: () => state,
    set: (patch) => {
      const p = typeof patch === 'function' ? patch(state) : patch
      let changed = false
      for (const k in p) if (!Object.is(state[k], p[k as keyof T])) changed = true
      if (!changed) return
      state = { ...state, ...p }
      subs.forEach((f) => f())
    },
    subscribe: (fn) => {
      subs.add(fn)
      return () => subs.delete(fn)
    },
  }
}

export function useStore<T extends object, S>(store: Store<T>, select: (s: T) => S): S {
  return useSyncExternalStore(store.subscribe, () => select(store.get()))
}
