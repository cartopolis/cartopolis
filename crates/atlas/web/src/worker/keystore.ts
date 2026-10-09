/**
 * The chat keys' storage: one IndexedDB object store, values are the state
 * blobs `ChatCore::take_state` returns.
 *
 * Its own database rather than the viewer's (`platform::storage`), because the
 * viewer's is an evictable cache in one namespace and a store of *data* in the
 * other, and these are private keys whose loss loses every chat on this device.
 * The page asks `navigator.storage.persist()` for it (`lib/chat.ts`): a worker
 * cannot, the method exists only on a window.
 */
import type {KeyStore} from './connection.ts'

const DB = 'cartopolis-chat'
const STORE = 'keys'

function open(): Promise<IDBDatabase> {
    return new Promise((resolve, reject) => {
        const req = indexedDB.open(DB, 1)
        req.onupgradeneeded = () => req.result.createObjectStore(STORE)
        req.onsuccess = () => resolve(req.result)
        req.onerror = () => reject(req.error)
    })
}

export class IndexedDbKeyStore implements KeyStore {
    private db: Promise<IDBDatabase> | null = null

    private conn(): Promise<IDBDatabase> {
        this.db ??= open()
        return this.db
    }

    async get(key: string): Promise<Uint8Array | undefined> {
        const db = await this.conn()
        return new Promise((resolve, reject) => {
            const req = db.transaction(STORE, 'readonly').objectStore(STORE).get(key)
            req.onsuccess = () => resolve(req.result instanceof Uint8Array ? req.result : undefined)
            req.onerror = () => reject(req.error)
        })
    }

    async put(key: string, value: Uint8Array): Promise<void> {
        const db = await this.conn()
        return new Promise((resolve, reject) => {
            const tx = db.transaction(STORE, 'readwrite')
            tx.objectStore(STORE).put(value, key)
            // `complete`, not the request's `success`: only then is it on disk.
            tx.oncomplete = () => resolve()
            tx.onerror = () => reject(tx.error)
            tx.onabort = () => reject(tx.error)
        })
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
