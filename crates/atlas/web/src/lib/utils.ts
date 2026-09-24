/**
 * Deep merge `source` into a copy of `target`, arrays and primitives replacing
 * rather than merging.
 *
 * An array is replaced, never concatenated: every array in this app's state is a
 * *set of rows from the server*, so merging a stale three-row list with a fresh
 * two-row one would resurrect the account that was just deleted.
 */
export function mergeDeep<T extends Record<string, unknown>>(
    target: T,
    source: Record<string, unknown>,
): T {
    const out: Record<string, unknown> = {...target}
    for (const [key, value] of Object.entries(source)) {
        const existing = out[key]
        if (isPlainObject(value) && isPlainObject(existing)) {
            out[key] = mergeDeep(existing as Record<string, unknown>, value)
        } else {
            out[key] = value
        }
    }
    return out as T
}

export function isPlainObject(value: unknown): value is Record<string, unknown> {
    return typeof value === 'object' && value !== null && !Array.isArray(value)
}

export function copyObject<T>(value: T): T {
    return structuredClone(value)
}

/**
 * An ISO-ish SQLite timestamp (`2026-08-13 09:12:44`) as something readable.
 *
 * SQLite's `datetime('now')` is UTC with a space where the `T` belongs, which
 * `Date` parses inconsistently across engines — Safari returns `Invalid Date`.
 * Normalising here rather than at the call sites, and falling back to the raw
 * string, because a date nobody can parse is still information and `Invalid Date`
 * is not.
 */
export function formatTimestamp(raw: string): string {
    if (!raw) {
        return '—'
    }
    const normalised = raw.includes('T') ? raw : `${raw.replace(' ', 'T')}Z`
    const date = new Date(normalised)
    if (Number.isNaN(date.getTime())) {
        return raw
    }
    return date.toLocaleString(undefined, {
        day: 'numeric',
        month: 'short',
        year: 'numeric',
    })
}
