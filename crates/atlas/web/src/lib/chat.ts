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
import type {Message} from '../worker/history.ts'
import type {
    ChatEvent,
    ChatSummary,
    Command,
    CommunityListing,
    CommunitySettings,
    CommunityView,
    Credential,
    Role,
    Update,
} from '../worker/protocol.ts'

export type {ChatEvent, ChatSummary, CommunityListing, CommunitySettings, CommunityView, Message, Role}

interface Port {
    postMessage(message: Command): void
}

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

    private port: Port
    private changed: () => void
    private listeners = new Set<(event: ChatEvent) => void>()

    /** `changed` is called whenever anything the page shows has changed. */
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
        this.send({type: 'sign_out'})
    }

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

    findCommunities(query: string, order: 'new' | 'popular'): void {
        this.send({order, query, type: 'find_communities'})
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
            case 'state':
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
        if (event.kind === 'notice' || event.kind === 'error') {
            this.notice = event.kind === 'notice' ? event.text : event.message
        }
        if (event.kind === 'refused') this.notice = event.message
        if (event.kind === 'line' && event.from !== this.address) {
            if (event.channel === this.current && !document.hidden) {
                // On screen: read as it arrives.
                this.send({channel: event.channel, type: 'open'})
            } else {
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
