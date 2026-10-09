/**
 * The page's handle on private chats: a thin client of the chat worker
 * (`src/worker/chat-worker.ts`), which holds the keys and the connection.
 *
 * A page never touches the keys. It sends commands and receives events and
 * the chat list; every open tab sees the same ones, because they all talk to
 * the same worker.
 */
import type {ChatEvent, ChatSummary, Command, Credential, Update} from '../worker/protocol.ts'

export type {ChatEvent, ChatSummary}

interface Port {
    postMessage(message: Command): void
}

export class ChatClient {
    chats: ChatSummary[] = []
    username: string | null = null
    private port: Port
    private listeners = new Set<(event: ChatEvent) => void>()
    private changed: () => void

    /** `changed` is called whenever the list or the signed-in name changes. */
    constructor(changed: () => void) {
        this.changed = changed
        const url = '/chat-worker.js'
        if (typeof SharedWorker !== 'undefined') {
            const worker = new SharedWorker(url, {name: 'cartopolis-chat', type: 'module'})
            worker.port.onmessage = (ev: MessageEvent<Update>) => this.receive(ev.data)
            worker.port.start()
            this.port = worker.port
        } else {
            // No SharedWorker (Chrome on Android): one worker per tab, and the
            // worker's Web Lock lets only one of them hold the keys at a time.
            const worker = new Worker(url, {name: 'cartopolis-chat', type: 'module'})
            worker.onmessage = (ev: MessageEvent<Update>) => this.receive(ev.data)
            this.port = worker
        }
    }

    onEvent(listener: (event: ChatEvent) => void): () => void {
        this.listeners.add(listener)
        return () => this.listeners.delete(listener)
    }

    signIn(credential: Credential): void {
        // The keys live in IndexedDB, which a browser may clear under storage
        // pressure unless the origin is persisted; only a page can ask (a worker
        // has no `persist`). Best effort: a refusal still stores, less durably.
        navigator.storage?.persist?.().catch(() => false)
        this.port.postMessage({credential, type: 'sign_in'})
    }

    signOut(): void {
        this.port.postMessage({type: 'sign_out'})
    }

    startDirect(username: string): void {
        this.port.postMessage({type: 'start_direct', username})
    }

    startGroup(name: string, members: string[]): void {
        this.port.postMessage({members, name, type: 'start_group'})
    }

    sendText(channel: string, body: string): void {
        this.port.postMessage({body, channel, type: 'send_text'})
    }

    sendPoint(channel: string, point: string): void {
        this.port.postMessage({channel, point, type: 'send_point'})
    }

    private receive(update: Update): void {
        if (update.type === 'chats') {
            this.chats = update.chats
            this.username = update.username
            this.changed()
        } else {
            for (const listener of this.listeners) listener(update.event)
        }
    }
}
