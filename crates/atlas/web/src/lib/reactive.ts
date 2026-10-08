import {state} from '@garage44/mithril'

/**
 * A component's own reactive state: what `useState` was under Preact.
 *
 * `state()` from Mithril, typed as the plain shape for the reason `Store.state`
 * is (`lib/store.ts`): the library's `State<T>` reads a nested object back as a
 * state with `$`-signal twins, so assigning a plain object or array to a field
 * would not type-check. A view that reads a field redraws when it is written —
 * from an event handler, a `fetch` that resolved, or a timer — with no
 * `m.redraw()` anywhere.
 */
export function local<T extends object>(initial: T): T {
    return state(initial) as unknown as T
}
