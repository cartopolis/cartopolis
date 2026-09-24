/**
 * An in-memory `localStorage` for the test runner.
 *
 * Only the four methods `Store` uses, and no more: a fuller fake would be a
 * second implementation of a browser API to keep correct, and the thing under
 * test is what the store chooses to write, not what the browser does with it.
 */
if (typeof globalThis.localStorage === 'undefined') {
    const entries = new Map<string, string>()
    Object.defineProperty(globalThis, 'localStorage', {
        value: {
            clear: () => entries.clear(),
            getItem: (key: string) => entries.get(key) ?? null,
            removeItem: (key: string) => entries.delete(key),
            setItem: (key: string, value: string) => entries.set(key, String(value)),
        },
        writable: true,
    })
}
