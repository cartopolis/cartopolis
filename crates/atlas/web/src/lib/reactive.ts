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

/**
 * One field of a `local()` (or `$s`) object as a component `model`.
 *
 * `local()` is typed as the plain shape, so its `$key` signals are invisible to
 * TypeScript; this is the typed way to hand a field to `FieldText` and friends.
 * The getter reads through the state proxy, so a view using it still subscribes,
 * and it is cheap enough to build in the view — but a `validation()` must be
 * built once (it holds whether the field was touched), so build those in the
 * constructor, from `bind` too.
 */
export function bind<T extends object, K extends keyof T>(s: T, key: K): {value: T[K]} {
    return {
        get value() {
            return s[key]
        },
        set value(next: T[K]) {
            s[key] = next
        },
    }
}
