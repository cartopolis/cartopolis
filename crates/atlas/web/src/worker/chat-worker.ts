/**
 * The chat worker: the one place in a browser that holds this account's chat
 * keys, its connection to the simulator, and what has been said in each chat
 * (`docs/notes/web-chat.md`).
 *
 * Loaded as a `SharedWorker`, so every tab shares one instance, and as a
 * dedicated `Worker` where there is no `SharedWorker` (Chrome on Android has
 * none). Either way it takes a **Web Lock** before it opens the keys. That is
 * the actual guarantee of one owner, and the SharedWorker is only the common
 * case of it: a deploy changes this script's URL, and a tab still running the
 * old one and a tab opened since would otherwise be two shared workers, two
 * copies of one device's keys, saving over each other.
 *
 * Pages hold no keys and no history of their own. They send commands and get
 * back three things: the state (chats, workspaces, who is signed in), a chat's
 * history when they open it, and every event as it happens.
 */
import init, {ChatCore} from './pkg/cartopolis_web_chat.js'
import {ChatConnection, type ChatCoreModule} from './connection.ts'
import {
    apply,
    changeLocally,
    type Chat,
    emptyChat,
    markRead,
    mentions,
    pending,
    reactLocally,
    unread,
} from './history.ts'
import {IndexedDbKeyStore} from './keystore.ts'
import type {ChatEvent, ChatSummary, Command, Credential, Update, WorkspaceView} from './protocol.ts'

declare const self: (SharedWorkerGlobalScope | DedicatedWorkerGlobalScope) & typeof globalThis

type Port = MessagePort | DedicatedWorkerGlobalScope

const ports = new Set<Port>()
const store = new IndexedDbKeyStore()
const ready = init({module_or_path: new URL('/chat-core.wasm', self.location.origin)})
// Typed against the interface `connection.ts` drives, so a change to the
// module's surface fails the worker typecheck rather than a browser.
const mod: ChatCoreModule = {ChatCore}

/** One name for every account: one connection per browser at a time. */
const LOCK = 'cartopolis-chat-keys'
/** How long changed histories wait before being written, in ms. */
const SAVE_AFTER_MS = 400

let conn: ChatConnection | null = null
let token: string | null = null
/** Lets go of the Web Lock this connection holds. */
let release: (() => void) | null = null
let status: 'connected' | 'connecting' | 'signed_out' | 'waiting' = 'signed_out'
/** This person's full address, once signed in. */
let me: string | null = null
let workspaces: WorkspaceView[] = []
const histories = new Map<string, Chat>()
const dirty = new Set<string>()
let saveTimer: ReturnType<typeof setTimeout> | null = null
/**
 * A `sent` that arrived before its send had been recorded as pending. Both
 * orders are possible: the send's answer and the server's acknowledgement are
 * separate steps of the connection's queue.
 */
const sentEarly = new Map<number, number>()
/** Events are handled one at a time, in order, like the connection's steps. */
let events: Promise<void> = Promise.resolve()

function post(update: Update, to?: Port): void {
    if (to) {
        to.postMessage(update)
        return
    }
    for (const port of ports) port.postMessage(update)
}

function chat(channel: string): Chat {
    let c = histories.get(channel)
    if (!c) {
        c = emptyChat()
        histories.set(channel, c)
    }
    return c
}

function changed(channel: string): void {
    dirty.add(channel)
    saveTimer ??= setTimeout(() => void save(), SAVE_AFTER_MS)
}

async function save(): Promise<void> {
    saveTimer = null
    if (!me) return
    const account = me
    const channels = [...dirty]
    dirty.clear()
    for (const channel of channels) {
        const c = histories.get(channel)
        if (c) await store.putHistory(account, channel, c).catch(() => changed(channel))
    }
}

function preview(c: Chat): {lastAtMs: number; preview: string} {
    for (let i = c.messages.length - 1; i >= 0; i--) {
        const m = c.messages[i]
        if (m.deleted || m.parent !== null) continue
        return {lastAtMs: m.sentAtMs, preview: m.body}
    }
    return {lastAtMs: 0, preview: ''}
}

function summaries(): ChatSummary[] {
    if (!conn) return []
    const who = me
    return conn.chats().map((c) => {
        const h = histories.get(c.channel) ?? emptyChat()
        const unseen = who ? unread(h, who) : 0
        return {
            ...c,
            ...preview(h),
            mentioned: unseen > 0 && !!who && h.messages.some(
                (m) => m.seq !== null && m.seq > h.readSeq && m.from !== who && mentions(m.body, who),
            ),
            unread: unseen,
        }
    })
}

function state(to?: Port): void {
    post({
        address: me,
        chats: summaries(),
        status,
        type: 'state',
        username: conn?.username ?? null,
        workspaces,
    }, to)
}

function history(channel: string, to?: Port): void {
    post({channel, messages: chat(channel).messages, type: 'history'}, to)
}

async function handle(event: ChatEvent): Promise<void> {
    switch (event.kind) {
        case 'signed_in':
            me = event.address
            status = 'connected'
            histories.clear()
            for (const [channel, c] of await store.histories(event.address).catch(() => new Map<string, Chat>())) {
                histories.set(channel, c)
            }
            break
        case 'line':
        case 'reaction':
        case 'edited':
        case 'deleted':
            if (me && apply(chat(event.channel), event, me)) {
                changed(event.channel)
                history(event.channel)
            }
            break
        case 'sent':
            if (me && apply(chat(event.channel), event, me)) {
                changed(event.channel)
                history(event.channel)
            } else {
                sentEarly.set(event.request, event.seq)
            }
            break
        case 'workspaces':
            workspaces = event.workspaces
            break
        case 'disconnected':
            if (status === 'connected') status = 'connecting'
            break
        case 'waiting_for_other_tab':
            status = 'waiting'
            break
        default:
            break
    }
    post({event, type: 'event'})
    state()
}

function onEvent(event: ChatEvent): void {
    events = events.then(() => handle(event)).catch(() => undefined)
}

/** Record this device's own message as it goes out. */
function sending(channel: string, request: number, body: string, parent: number | null): void {
    if (!me) return
    const c = chat(channel)
    pending(c, request, me, body, parent, Date.now())
    const seq = sentEarly.get(request)
    if (seq !== undefined) {
        sentEarly.delete(request)
        apply(c, {channel, kind: 'sent', request, seq}, me)
    }
    changed(channel)
    history(channel)
}

function signOut(): void {
    conn?.stop()
    conn = null
    token = null
    me = null
    status = 'signed_out'
    workspaces = []
    histories.clear()
    release?.()
    release = null
    state()
}

async function signIn(credential: Credential): Promise<void> {
    const key = 'token' in credential ? credential.token : `${credential.username}:${credential.password}`
    if (conn && token === key) {
        state()
        return
    }
    signOut()
    token = key
    status = 'connecting'
    state()
    await ready
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
        status = 'connecting'
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

async function run(command: Command, from: Port): Promise<void> {
    if (command.type === 'sign_in') return signIn(command.credential)
    if (command.type === 'sign_out') return signOut()
    if (command.type === 'open') {
        if (markRead(chat(command.channel))) {
            changed(command.channel)
            state()
        }
        history(command.channel, from)
        return
    }
    if (!conn) throw new Error('not signed in to chat')
    const c = conn
    switch (command.type) {
        case 'start_direct':
            return c.startDirect(command.username)
        case 'start_group':
            return c.startGroup(command.name, command.members)
        case 'send_text': {
            const request = await c.sendText(command.channel, command.body, command.parent)
            sending(command.channel, request, command.body, command.parent)
            return
        }
        case 'send_point':
            await c.sendPoint(command.channel, command.point)
            return
        case 'react':
            await c.react(command.channel, command.target, command.emoji, command.remove)
            if (me) reactLocally(chat(command.channel), command.target, command.emoji, me, command.remove)
            changed(command.channel)
            history(command.channel)
            return
        case 'edit':
            await c.edit(command.channel, command.target, command.body)
            if (me) changeLocally(chat(command.channel), command.target, me, command.body)
            changed(command.channel)
            history(command.channel)
            return
        case 'delete':
            await c.remove(command.channel, command.target)
            if (me) changeLocally(chat(command.channel), command.target, me, null)
            changed(command.channel)
            history(command.channel)
            return
        case 'create_workspace':
            return c.createWorkspace(command.name)
        case 'add_workspace_member':
            return c.addWorkspaceMember(command.workspace, command.who)
        case 'start_channel':
            return c.startChannel(command.workspace, command.name, command.topic, command.private)
        case 'join_channel':
            return c.joinChannel(command.workspace, command.channel)
        case 'add_to_chat':
            return c.addToChat(command.channel, command.who)
    }
}

function attach(port: Port): void {
    ports.add(port)
    port.onmessage = (ev: MessageEvent<Command>) => {
        run(ev.data, port).catch((e) => port.postMessage({
            event: {kind: 'error', message: String(e?.message ?? e)},
            type: 'event',
        } satisfies Update))
    }
    state(port)
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
