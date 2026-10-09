/**
 * The chat worker: the one place in a browser that holds this account's chat
 * keys and its connection to the simulator (`docs/notes/web-chat.md`).
 *
 * Loaded as a `SharedWorker`, so every tab shares one instance, and as a
 * dedicated `Worker` where there is no `SharedWorker` (Chrome on Android has
 * none). Either way it takes a **Web Lock** before it opens the keys. That is
 * the actual guarantee of one owner, and the SharedWorker is only the common
 * case of it: a deploy changes this script's URL, and a tab still running the
 * old one and a tab opened since would otherwise be two shared workers, two
 * copies of one device's keys, saving over each other.
 */
import init, {ChatCore} from './pkg/cartopolis_web_chat.js'
import {ChatConnection, type ChatCoreModule} from './connection.ts'
import {IndexedDbKeyStore} from './keystore.ts'
import type {ChatEvent, Command, Credential, Update} from './protocol.ts'

declare const self: (SharedWorkerGlobalScope | DedicatedWorkerGlobalScope) & typeof globalThis

const ports = new Set<MessagePort | DedicatedWorkerGlobalScope>()
const store = new IndexedDbKeyStore()
const ready = init({module_or_path: new URL('/chat-core.wasm', self.location.origin)})
// Typed against the interface `connection.ts` drives, so a change to the
// module's surface fails the worker typecheck rather than a browser.
const mod: ChatCoreModule = {ChatCore}

/** One name for every account: one connection per browser at a time. */
const LOCK = 'cartopolis-chat-keys'

let conn: ChatConnection | null = null
let token: string | null = null
/** Lets go of the Web Lock this connection holds. */
let release: (() => void) | null = null

function broadcast(update: Update): void {
    for (const port of ports) port.postMessage(update)
}

function summary(): Update {
    return {chats: conn?.chats() ?? [], type: 'chats', username: conn?.username ?? null}
}

function onEvent(event: ChatEvent): void {
    broadcast({event, type: 'event'})
    // Anything that can change the list sends the list again; it is small.
    if (['signed_in', 'opened', 'named', 'disconnected'].includes(event.kind)) {
        broadcast(summary())
    }
}

function signOut(): void {
    conn?.stop()
    conn = null
    token = null
    release?.()
    release = null
    broadcast(summary())
}

async function signIn(credential: Credential): Promise<void> {
    const key = 'token' in credential ? credential.token : `${credential.username}:${credential.password}`
    if (conn && token === key) {
        return
    }
    signOut()
    token = key
    await ready
    // The lock is per browser profile and per origin; one name covers every
    // account, which is the simple and safe choice: one connection at a time.
    // Held until `release` is called, because the callback's promise is the lock.
    const hold = (resolve: () => void) => () => {
        resolve()
        return new Promise<void>((r) => (release = r))
    }
    const got = await new Promise<boolean>((resolve) => {
        navigator.locks.request(LOCK, {ifAvailable: true}, (lock) =>
            lock ? hold(() => resolve(true))() : resolve(false))
    })
    if (!got) {
        onEvent({kind: 'waiting_for_other_tab'})
        await new Promise<void>((resolve) => navigator.locks.request(LOCK, hold(resolve)))
    }
    if (token !== key) {
        // Signed out (or in as someone else) while waiting for the lock.
        release?.()
        return
    }
    const scheme = self.location.protocol === 'https:' ? 'wss:' : 'ws:'
    conn = new ChatConnection(mod, {
        credential,
        onEvent,
        server: self.location.host,
        store,
        url: `${scheme}//${self.location.host}/ws`,
    })
    conn.start()
}

async function run(command: Command): Promise<void> {
    switch (command.type) {
        case 'sign_in':
            return signIn(command.credential)
        case 'sign_out':
            return signOut()
        case 'start_direct':
            return conn?.startDirect(command.username)
        case 'start_group':
            return conn?.startGroup(command.name, command.members)
        case 'send_text':
            return conn?.sendText(command.channel, command.body)
        case 'send_point':
            return conn?.sendPoint(command.channel, command.point)
    }
}

function attach(port: MessagePort | DedicatedWorkerGlobalScope): void {
    ports.add(port)
    port.onmessage = (ev: MessageEvent<Command>) => {
        run(ev.data).catch((e) => port.postMessage({
            event: {kind: 'error', message: String(e?.message ?? e)},
            type: 'event',
        } satisfies Update))
    }
    port.postMessage(summary())
}

if ('onconnect' in self) {
    (self as SharedWorkerGlobalScope).onconnect = (ev: MessageEvent) => {
        const port = ev.ports[0]
        attach(port)
        port.start()
    }
} else {
    attach(self as DedicatedWorkerGlobalScope)
}
