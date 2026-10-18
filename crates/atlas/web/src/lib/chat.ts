/**
 * The page's handle on chat: a thin client of the chat worker
 * (`src/worker/chat-worker.ts`), which holds the keys, the connection and the
 * history.
 *
 * A page never touches the keys. It sends commands and keeps what the worker
 * tells it — the chat list, the communities, the history of whichever chat it
 * opened — and every open tab sees the same, because they all talk to the same
 * worker.
 */
import {WebPush} from './web-push'
import type {Message} from '../worker/history.ts'
import type {
    ChatEvent,
    ChatSummary,
    Command,
    CommunityListing,
    CommunityPlace,
    CommunitySettings,
    CommunityView,
    Credential,
    Role,
    Update,
} from '../worker/protocol.ts'

export type {ChatEvent, ChatSummary, CommunityListing, CommunityPlace, CommunitySettings, CommunityView, Message, Role}

interface Port {
    postMessage(message: Command): void
}

import {Calls} from './call'
import {type MeetingObject, caption, toJson} from './meeting'

/** Where muted topics are kept: a choice about this browser, told to nobody. */
const MUTED_KEY = 'cartopolis.chat.muted'

export type Status = 'connected' | 'connecting' | 'signed_out' | 'waiting'

export class ChatClient {
    address: string | null = null
    chats: ChatSummary[] = []
    status: Status = 'signed_out'
    username: string | null = null
    communities: CommunityView[] = []
    /** The last directory search's answer. */
    directory: CommunityListing[] = []
    /** The last thing the worker or the server refused, for the page to show. */
    notice: string | null = null
    /** Histories of the chats this page has opened, by channel key. */
    readonly histories = new Map<string, Message[]>()
    /** The chat this page is showing, if any: its new lines count as read. */
    current: string | null = null
    /** The last invite link made here: the community and its code. */
    invite: {code: string; id: string} | null = null
    /** Topics muted in this browser, by channel: no count and no notification. */
    readonly muted = new Set<string>(
        JSON.parse((typeof localStorage === 'undefined' ? null : localStorage.getItem(MUTED_KEY)) ?? '[]') as string[],
    )

    /** The call this page runs, and the calls ringing in its chats. */
    readonly calls: Calls
    private keyAsks = new Map<number, (keys: {epoch: number; index: number; key: string} | null) => void>()
    private nextAsk = 1

    private port: Port
    private changed: () => void
    private listeners = new Set<(event: ChatEvent) => void>()

    /** `changed` is called whenever anything the page shows has changed. */
    constructor(changed: () => void) {
        this.changed = changed
        this.calls = new Calls(this, changed)
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

    private send(command: Command): void {
        this.port.postMessage(command)
    }

    signIn(credential: Credential): void {
        // The keys and the history live in IndexedDB, which a browser may clear
        // under storage pressure unless the origin is persisted; only a page can
        // ask (a worker has no `persist`). Best effort: a refusal still stores,
        // less durably.
        navigator.storage?.persist?.().catch(() => false)
        this.send({credential, type: 'sign_in'})
    }

    signOut(): void {
        // Before the sign-out, on the same connection: the server takes back
        // only an address the connection asking registered.
        void this.push.disable()
        this.send({type: 'sign_out'})
    }

    /** Rings this browser for a call while the page is closed (`web-push.ts`). */
    readonly push = new WebPush((endpoint) => this.send({endpoint, type: 'push_endpoint'}))

    /** Show a chat: its history arrives, and it counts as read. */
    open(channel: string | null): void {
        this.current = channel
        if (channel) this.send({channel, type: 'open'})
    }

    startDirect(username: string): void {
        this.send({type: 'start_direct', username})
    }

    startGroup(name: string, members: string[]): void {
        this.send({members, name, type: 'start_group'})
    }

    sendText(channel: string, body: string, parent: number | null = null): void {
        this.send({body, channel, parent, type: 'send_text'})
    }

    /** Send a plan, an answer, a poll, a vote, a note or a record (`meeting.ts`). */
    sendMeeting(channel: string, object: MeetingObject): void {
        this.send({body: caption(object), channel, object: toJson(object), type: 'send_meeting'})
    }

    react(channel: string, target: number, emoji: string, remove: boolean): void {
        this.send({channel, emoji, remove, target, type: 'react'})
    }

    edit(channel: string, target: number, body: string): void {
        this.send({body, channel, target, type: 'edit'})
    }

    remove(channel: string, target: number): void {
        this.send({channel, target, type: 'delete'})
    }

    createCommunity(settings: CommunitySettings, topics: string[], people: string[]): void {
        this.send({people, settings, topics, type: 'create_community'})
    }

    editCommunity(id: string, settings: CommunitySettings): void {
        this.send({id, settings, type: 'edit_community'})
    }

    /** Search the directory; `near` is where the person is, for `near`. */
    findCommunities(query: string, order: 'near' | 'new' | 'popular', near: [number, number] | null = null): void {
        this.send({latitude: near?.[0] ?? null, longitude: near?.[1] ?? null, order, query, type: 'find_communities'})
    }

    createInvite(id: string): void {
        this.send({id, type: 'create_invite'})
    }

    /** Look an invite up: the community arrives as the directory's one entry. */
    lookupInvite(code: string): void {
        this.directory = []
        this.send({code, type: 'lookup_invite'})
    }

    joinInvite(code: string): void {
        this.send({code, type: 'join_invite'})
    }

    isMuted(channel: string): boolean {
        return this.muted.has(channel)
    }

    setMuted(channel: string, muted: boolean): void {
        if (muted) this.muted.add(channel)
        else this.muted.delete(channel)
        if (typeof localStorage !== 'undefined') localStorage.setItem(MUTED_KEY, JSON.stringify([...this.muted]))
        this.changed()
    }

    joinCommunity(id: string, message: string): void {
        this.send({id, message, type: 'join_community'})
    }

    leaveCommunity(id: string): void {
        this.send({id, type: 'leave_community'})
    }

    decideRequest(id: string, who: string, accept: boolean): void {
        this.send({accept, id, type: 'decide_request', who})
    }

    addToCommunity(id: string, who: string): void {
        this.send({id, type: 'add_to_community', who})
    }

    setCommunityRole(id: string, who: string, role: Role): void {
        this.send({id, role, type: 'set_community_role', who})
    }

    /** Take somebody out of one topic (`channel`), or the whole community (`''`). */
    removeFromCommunity(id: string, who: string, channel: string, block: boolean): void {
        this.send({block, channel, id, type: 'remove_from_community', who})
    }

    deleteCommunity(id: string): void {
        this.send({id, type: 'delete_community'})
    }

    startTopic(id: string, name: string): void {
        this.send({id, name, type: 'start_topic'})
    }

    editTopic(id: string, channel: string, name: string, position: number, archived: boolean): void {
        this.send({archived, channel, id, name, position, type: 'edit_topic'})
    }

    moderateDelete(channel: string, target: number): void {
        this.send({channel, target, type: 'moderate_delete'})
    }

    reportToModerators(channel: string, target: number, who: string, reason: string, note: string, line: string): void {
        this.send({channel, line, note, reason, target, type: 'report_to_moderators', who})
    }

    resolveReport(id: string, report: number): void {
        this.send({id, report, type: 'resolve_report'})
    }

    /** The chat's call key for its current epoch, from the worker that holds the keys. */
    callKeys(channel: string): Promise<{epoch: number; index: number; key: string} | null> {
        const id = this.nextAsk++
        return new Promise((resolve) => {
            this.keyAsks.set(id, resolve)
            this.send({channel, id, type: 'call_keys'})
        })
    }

    /** Call signalling, carried by the worker's connection. */
    callSignal(
        command: Extract<
            Command,
            {
                type:
                    | 'call_answer'
                    | 'call_control'
                    | 'call_decline'
                    | 'call_invite'
                    | 'call_join'
                    | 'call_key_drop'
                    | 'call_leave'
                    | 'call_react'
                    | 'call_watch'
            }
        >,
    ): void {
        this.send(command)
    }

    /** The community a chat is a topic of, and the topic. */
    topicOf(channel: string): {community: CommunityView; topic: CommunityView['topics'][number]} | null {
        for (const community of this.communities) {
            const topic = community.topics.find((t) => t.channel === channel)
            if (topic) return {community, topic}
        }
        return null
    }

    addToChat(channel: string, who: string): void {
        this.send({channel, type: 'add_to_chat', who})
    }

    private receive(update: Update): void {
        switch (update.type) {
            case 'call_keys':
                this.keyAsks.get(update.id)?.(update.keys)
                this.keyAsks.delete(update.id)
                return
            case 'state':
                // Each connection registers this browser's push address anew:
                // it is what the server keys "take it back" on, and it moves
                // the address to whoever is signed in here now.
                if (update.status === 'connected' && this.status !== 'connected') void this.push.enable()
                this.address = update.address
                this.chats = update.chats
                this.status = update.status
                this.username = update.username
                this.communities = update.communities
                this.directory = update.directory
                break
            case 'history':
                this.histories.set(update.channel, update.messages)
                break
            case 'event':
                this.onWorkerEvent(update.event)
                break
        }
        this.changed()
    }

    private onWorkerEvent(event: ChatEvent): void {
        if (event.kind.startsWith('call_')) this.calls.onEvent(event as {kind: string})
        if (event.kind === 'notice' || event.kind === 'error') {
            this.notice = event.kind === 'notice' ? event.text : event.message
        }
        if (event.kind === 'refused') this.notice = event.message
        if (event.kind === 'invite') this.invite = {code: event.code, id: event.id}
        if (event.kind === 'line' && event.from !== this.address) {
            if (event.channel === this.current && !document.hidden) {
                // On screen: read as it arrives.
                this.send({channel: event.channel, type: 'open'})
            } else if (!this.muted.has(event.channel)) {
                notify(this, event)
            }
        }
        for (const listener of this.listeners) listener(event)
    }
}

/**
 * Tell the person about a line they are not looking at, through the browser's
 * notifications — only once they have allowed them, which the chat page asks
 * for on the first click rather than on load (browsers refuse an ask that no
 * gesture made).
 */
function notify(client: ChatClient, event: Extract<ChatEvent, {kind: 'line'}>): void {
    if (typeof Notification === 'undefined' || Notification.permission !== 'granted') return
    const chat = client.chats.find((c) => c.channel === event.channel)
    const who = event.from.split('@')[0]
    const where = chat && chat.group ? `${who} in ${chat.label}` : who
    const n = new Notification(where, {body: event.body.slice(0, 200), tag: event.channel})
    n.onclick = () => {
        window.focus()
        client.open(event.channel)
        n.close()
    }
}
