import {type DeepSignal, deepSignal} from 'deepsignal'

import {copyObject, mergeDeep} from './utils'

/**
 * The whole app's state, as one deepsignal proxy.
 *
 * Codebrew's model, and the reason it is worth copying: there is no context, no
 * provider and no selector. A component that reads `$s.accounts` subscribes to
 * exactly that, and writing `$s.accounts = […]` from an API call re-renders the
 * components that read it and nothing else.
 *
 * `persistentState` does double duty. It is the initial shape *and* the
 * whitelist for `save()`: `filterKeys` walks it as a blueprint, so a key that is
 * not in it is structurally incapable of reaching localStorage. That is what
 * keeps the session token's neighbours — the account list, an in-flight form —
 * out of a store that survives the tab.
 */
export class Store<State extends Record<string, unknown>> {
    state: DeepSignal<State>

    private blueprint?: State

    private key: string

    constructor(key = 'cartopolis-admin') {
        this.key = key
        this.state = deepSignal({} as State)
    }

    load(persistentState: State, volatileState: Partial<State>) {
        this.blueprint = copyObject(persistentState)

        let restored: Record<string, unknown> = {}
        try {
            restored = JSON.parse(localStorage.getItem(this.key) || '{}') as Record<string, unknown>
        } catch {
            // A corrupt store is not worth a broken app, and there is nothing in
            // it that cannot be fetched again.
            restored = {}
        }

        Object.assign(
            this.state,
            mergeDeep(mergeDeep(persistentState, restored), volatileState as Record<string, unknown>),
        )
    }

    /**
     * Keep only the keys present in `blueprint`, recursing into nested objects.
     * A key absent from the blueprint is dropped even if it exists in `obj`.
     */
    filterKeys(obj: Record<string, unknown>, blueprint: Record<string, unknown>): Record<string, unknown> {
        const result: Record<string, unknown> = {}
        for (const key of Object.keys(blueprint)) {
            if (!Object.hasOwn(obj, key)) {
                continue
            }
            const shape = blueprint[key]
            const value = obj[key]
            if (shape !== null && typeof shape === 'object' && !Array.isArray(shape) && value !== null &&
                typeof value === 'object') {
                result[key] = this.filterKeys(value as Record<string, unknown>, shape as Record<string, unknown>)
            } else {
                result[key] = value
            }
        }
        return result
    }

    save() {
        if (!this.blueprint) {
            return
        }
        const persisted = this.filterKeys(
            this.state as unknown as Record<string, unknown>,
            this.blueprint as Record<string, unknown>,
        )
        try {
            localStorage.setItem(this.key, JSON.stringify(persisted))
        } catch {
            // Private-mode Safari throws on every write. Losing the remembered
            // session is not a reason to lose the click that triggered the save.
        }
    }

    clear() {
        try {
            localStorage.removeItem(this.key)
        } catch {
            // As above.
        }
    }
}
