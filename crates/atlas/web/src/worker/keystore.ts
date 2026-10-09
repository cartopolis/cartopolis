/**
 * The chat worker's storage: one IndexedDB database with two object stores.
 *
 * - `keys`: the state blobs `ChatCore::take_state` returns — private keys and
 *   every chat's ratchet position.
 * - `history`: what has been said in each chat (`history.ts`), as this device
 *   read it. The server keeps only sealed bytes and replays from where the
 *   device left off, so this is the only readable copy there is.
 *
 * Its own database rather than the viewer's (`platform::storage`), because the
 * viewer's is an evictable cache in one namespace and a store of *data* in the
 * other, and these are private keys whose loss loses every chat on this device.
 * The page asks `navigator.storage.persist()` for it (`lib/chat.ts`): a worker
 * cannot, the method exists only on a window.
 */
import type {KeyStore} from './connection.ts'
import type {Chat} from './history.ts'

const DB = 'cartopolis-chat'
const KEYS = 'keys'
const HISTORY = 'history'

function open(): Promise<IDBDatabase> {
    return new Promise((resolve, reject) => {
        // Version 2 added `history`; version 1 had only the keys.
        const req = indexedDB.open(DB, 2)
        req.onupgradeneeded = () => {
            const db = req.result
            if (!db.objectStoreNames.contains(KEYS)) db.createObjectStore(KEYS)
            if (!db.objectStoreNames.contains(HISTORY)) db.createObjectStore(HISTORY)
        }
        req.onsuccess = () => resolve(req.result)
        req.onerror = () => reject(req.error)
    })
}

/** Where a chat's history is kept: per account, per chat. */
export function historyKey(account: string, channel: string): string {
    return `${account}|${channel}`
}

export class IndexedDbKeyStore implements KeyStore {
    private db: Promise<IDBDatabase> | null = null

    private conn(): Promise<IDBDatabase> {
        this.db ??= open()
        return this.db
    }

    private async read<T>(store: string, key: string): Promise<T | undefined> {
        const db = await this.conn()
        return new Promise((resolve, reject) => {
            const req = db.transaction(store, 'readonly').objectStore(store).get(key)
            req.onsuccess = () => resolve(req.result as T | undefined)
            req.onerror = () => reject(req.error)
        })
    }

    private async write(store: string, key: string, value: unknown): Promise<void> {
        const db = await this.conn()
        return new Promise((resolve, reject) => {
            const tx = db.transaction(store, 'readwrite')
            tx.objectStore(store).put(value, key)
            // `complete`, not the request's `success`: only then is it on disk.
            tx.oncomplete = () => resolve()
            tx.onerror = () => reject(tx.error)
            tx.onabort = () => reject(tx.error)
        })
    }

    async get(key: string): Promise<Uint8Array | undefined> {
        const v = await this.read<unknown>(KEYS, key)
        return v instanceof Uint8Array ? v : undefined
    }

    put(key: string, value: Uint8Array): Promise<void> {
        return this.write(KEYS, key, value)
    }

    /** Every chat's history this account has, by channel key. */
    async histories(account: string): Promise<Map<string, Chat>> {
        const db = await this.conn()
        const prefix = `${account}|`
        return new Promise((resolve, reject) => {
            const out = new Map<string, Chat>()
            const range = IDBKeyRange.bound(prefix, `${prefix}￿`)
            const req = db.transaction(HISTORY, 'readonly').objectStore(HISTORY).openCursor(range)
            req.onsuccess = () => {
                const cursor = req.result
                if (!cursor) {
                    resolve(out)
                    return
                }
                out.set(String(cursor.key).slice(prefix.length), cursor.value as Chat)
                cursor.continue()
            }
            req.onerror = () => reject(req.error)
        })
    }

    putHistory(account: string, channel: string, chat: Chat): Promise<void> {
        return this.write(HISTORY, historyKey(account, channel), chat)
    }
}

/** For tests and Bun: the same contract, in memory. */
export class MemoryKeyStore implements KeyStore {
    readonly values = new Map<string, Uint8Array>()

    async get(key: string): Promise<Uint8Array | undefined> {
        return this.values.get(key)
    }

    async put(key: string, value: Uint8Array): Promise<void> {
        this.values.set(key, value.slice())
    }
}
