/**
 * One device's private chats over one connection to the simulator.
 *
 * Drives the Rust engine (`crates/web_chat`, compiled to WebAssembly) through a
 * WebSocket: frames in, frames out, the device's state saved after every step.
 * It owns no globals and knows nothing about workers, so the same class runs in
 * the shared worker (`chat-worker.ts`) and in `chat-e2e.ts`, which drives two of
 * them against a real simulator from Bun.
 *
 * Two orderings are load-bearing and both are about the keys:
 *
 *  - **Everything is applied in arrival order, one step at a time.** Opening the
 *    keys awaits storage, and frames that arrive meanwhile must wait behind it:
 *    a commit applied out of order breaks the group it belongs to.
 *  - **The state is written before anything is sent.** A step can advance a
 *    group (sending a commit is one); if what it sent reached the server and the
 *    new state never reached storage, the next load is an epoch behind every
 *    other member, which is unrecoverable.
 */
import type {ChatEvent, Credential} from './protocol.ts'

/** The slice of `ChatCore` (wasm-bindgen's class) this file uses. */
export interface ChatCoreApi {
    add_to_chat(channel: string, who: string): void
    add_workspace_member(workspace: string, who: string): void
    chats(): string
    create_workspace(name: string): void
    free(): void
    is_open(): boolean
    join_channel(workspace: string, channel: string): void
    list_workspaces(): void
    next_frame(): Uint8Array | undefined
    open(saved?: Uint8Array | null): void
    receive(frame: Uint8Array): void
    // Every send answers the request id it went out under (see `Sent`).
    send_delete(channel: string, target: number, nowMs: number): number
    send_edit(channel: string, target: number, body: string, nowMs: number): number
    send_point(channel: string, pointJson: string, nowMs: number): number
    send_reaction(channel: string, target: number, emoji: string, remove: boolean, nowMs: number): number
    send_reply(channel: string, parent: number, body: string, nowMs: number): number
    send_text(channel: string, body: string, nowMs: number): number
    start_channel(workspace: string, name: string, topic: string, isPrivate: boolean): void
    start_direct(username: string): void
    start_group(name: string, members: string[]): void
    take_events(): string
    take_state(): Uint8Array | undefined
    username(): string | undefined
    /** The same person as a full address, `name@server` (protocol 37). */
    address(): string | undefined
}

/** The module's exports, as `wasm-bindgen --target web` writes them. */
export interface ChatCoreModule {
    ChatCore: {
        hello_password(username: string, password: string): Uint8Array
        hello_token(token: string): Uint8Array
        new (): ChatCoreApi
        protocol_version(): number
    }
}

/** Where a device's state is kept. Holds private keys: never a cache. */
export interface KeyStore {
    get(key: string): Promise<Uint8Array | undefined>
    put(key: string, value: Uint8Array): Promise<void>
}

export interface ConnectionOptions {
    credential: Credential
    /** Called with every event, in order. */
    onEvent: (event: ChatEvent) => void
    /**
     * Which server, for the storage key: the same person on two servers is two
     * unrelated sets of keys. The page's origin, in the web UI.
     */
    server: string
    store: KeyStore
    /** `wss://…/ws`. */
    url: string
}

/**
 * The storage key for one account's keys. Not the viewer's (`e2ee/…` in its own
 * store): until the two share one connection they are two devices, and two
 * devices must never share one state.
 */
export function stateKey(server: string, username: string): string {
    const clean = (s: string) => s.replace(/[^A-Za-z0-9-]/g, '_')
    return `web-chat/${clean(server)}/${clean(username.toLowerCase())}`
}

/** Waits before reconnecting, in ms: 1 s doubling to 30 s. */
const BACKOFF = [1000, 2000, 4000, 8000, 16000, 30000]

export class ChatConnection {
    private core: ChatCoreApi | null = null
    private mod: ChatCoreModule
    private opts: ConnectionOptions
    /** The tail of the work queue; every step is chained onto it. */
    private queue: Promise<void> = Promise.resolve()
    private socket: WebSocket | null = null
    private stopped = false
    private attempts = 0

    constructor(mod: ChatCoreModule, opts: ConnectionOptions) {
        this.mod = mod
        this.opts = opts
    }

    /** Connect, and keep reconnecting until `stop` or a refusal. */
    start(): void {
        this.stopped = false
        this.connect()
    }

    stop(): void {
        this.stopped = true
        this.socket?.close()
        this.socket = null
    }

    get username(): string | undefined {
        return this.core?.username()
    }

    /** Every chat this device is in. Empty until the keys are open. */
    chats(): {channel: string; group: boolean; label: string; people: string[]}[] {
        return this.core?.is_open() ? JSON.parse(this.core.chats()) : []
    }

    startDirect(username: string): Promise<void> {
        return this.step((core) => core.start_direct(username))
    }

    startGroup(name: string, members: string[]): Promise<void> {
        return this.step((core) => core.start_group(name, members))
    }

    /**
     * Say something in a chat, or in the thread under `parent`. Answers the
     * request id, which a `sent` event later pairs with the message's id.
     */
    sendText(channel: string, body: string, parent: number | null = null): Promise<number> {
        return this.step((core) => parent === null
            ? core.send_text(channel, body, Date.now())
            : core.send_reply(channel, parent, body, Date.now()))
    }

    sendPoint(channel: string, pointJson: string): Promise<number> {
        return this.step((core) => core.send_point(channel, pointJson, Date.now()))
    }

    react(channel: string, target: number, emoji: string, remove: boolean): Promise<number> {
        return this.step((core) => core.send_reaction(channel, target, emoji, remove, Date.now()))
    }

    edit(channel: string, target: number, body: string): Promise<number> {
        return this.step((core) => core.send_edit(channel, target, body, Date.now()))
    }

    remove(channel: string, target: number): Promise<number> {
        return this.step((core) => core.send_delete(channel, target, Date.now()))
    }

    createWorkspace(name: string): Promise<void> {
        return this.step((core) => core.create_workspace(name))
    }

    addWorkspaceMember(workspace: string, who: string): Promise<void> {
        return this.step((core) => core.add_workspace_member(workspace, who))
    }

    startChannel(workspace: string, name: string, topic: string, isPrivate: boolean): Promise<void> {
        return this.step((core) => core.start_channel(workspace, name, topic, isPrivate))
    }

    joinChannel(workspace: string, channel: string): Promise<void> {
        return this.step((core) => core.join_channel(workspace, channel))
    }

    addToChat(channel: string, who: string): Promise<void> {
        return this.step((core) => core.add_to_chat(channel, who))
    }

    /** This person's full address, once signed in. */
    get address(): string | undefined {
        return this.core?.address()
    }

    /** Wait for everything queued so far. */
    settled(): Promise<void> {
        return this.queue
    }

    private connect(): void {
        if (this.stopped) return
        // A fresh engine per connection: the state lives in the store between
        // them, and a half-finished handshake must not leak into the next one.
        this.core?.free()
        const core = new this.mod.ChatCore()
        this.core = core
        const socket = new WebSocket(this.opts.url)
        socket.binaryType = 'arraybuffer'
        this.socket = socket

        socket.onopen = () => {
            const c = this.opts.credential
            socket.send('token' in c
                ? this.mod.ChatCore.hello_token(c.token)
                : this.mod.ChatCore.hello_password(c.username, c.password))
        }
        socket.onmessage = (ev: MessageEvent) => {
            if (!(ev.data instanceof ArrayBuffer)) return
            const frame = new Uint8Array(ev.data)
            this.enqueue(async () => {
                if (this.core !== core) return
                core.receive(frame)
                await this.afterStep(core)
            })
        }
        socket.onclose = () => {
            if (this.socket !== socket) return
            this.socket = null
            this.opts.onEvent({kind: 'disconnected'})
            if (this.stopped) return
            const wait = BACKOFF[Math.min(this.attempts, BACKOFF.length - 1)]
            this.attempts += 1
            setTimeout(() => this.connect(), wait)
        }
    }

    /** A step the caller asked for; rejects if the keys are not open. */
    private step<T>(f: (core: ChatCoreApi) => T): Promise<T> {
        return this.enqueue(async () => {
            const core = this.core
            if (!core?.is_open()) throw new Error('chats are not connected yet')
            const answer = f(core)
            await this.afterStep(core)
            return answer
        })
    }

    private enqueue<T>(f: () => Promise<T>): Promise<T> {
        const run = this.queue.then(f)
        // The queue itself never rejects, or one failed step would stall every
        // later frame; the caller still sees its own failure through `run`.
        this.queue = run.then(
            () => undefined,
            (e) => this.opts.onEvent({kind: 'error', message: String(e?.message ?? e)}),
        )
        return run
    }

    /** Save, then send, then report: the order the module note explains. */
    private async afterStep(core: ChatCoreApi): Promise<void> {
        const events: ChatEvent[] = JSON.parse(core.take_events())
        for (const event of events) {
            if (event.kind === 'signed_in') {
                this.attempts = 0
                await this.openKeys(core, event.username)
            }
            if (event.kind === 'refused') {
                // A bad or expired token: retrying would be refused again forever.
                this.stopped = true
            }
        }
        const state = core.take_state()
        if (state) {
            await this.opts.store.put(stateKey(this.opts.server, core.username() ?? ''), state)
        }
        for (let frame = core.next_frame(); frame; frame = core.next_frame()) {
            this.socket?.send(frame)
        }
        for (const event of events) this.opts.onEvent(event)
        // `openKeys` may have produced events of its own (none today, but the
        // engine's on-connect could grow some); never drop them.
        const more: ChatEvent[] = JSON.parse(core.take_events())
        for (const event of more) this.opts.onEvent(event)
    }

    private async openKeys(core: ChatCoreApi, username: string): Promise<void> {
        const saved = await this.opts.store.get(stateKey(this.opts.server, username))
        try {
            core.open(saved ?? null)
            // The workspaces come with being signed in: every page wants them.
            core.list_workspaces()
        } catch (e) {
            // Never start a new device over keys that would not load: that would
            // orphan every chat they were in. Say so and stay closed.
            this.stopped = true
            this.socket?.close()
            throw new Error(`could not open this browser's chat keys: ${(e as Error).message}`)
        }
    }
}
