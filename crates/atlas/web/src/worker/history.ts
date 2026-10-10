/**
 * What has been said in each chat, as this device saw it.
 *
 * The server keeps only sealed bytes and replays a chat from where this device
 * left off, so the readable history exists nowhere but here: the worker applies
 * every event to it and keeps it in IndexedDB beside the keys. Pure functions
 * over plain data, so `history.test.ts` pins the rules without a browser.
 *
 * A message's id is its place in the chat's log (`seq`), the same on every
 * member's device. Until the server has given this device's own message one,
 * it carries the request id it went out under (`local`), and `sent` swaps the
 * one for the other.
 */
import type {ChatEvent} from './protocol.ts'

export interface Picture {
    hash: string
    height: number
    key: string
    nonce: string
    width: number
}

export interface Message {
    /** Its place in the log, once the server has given it one. */
    seq: number | null
    /** While `seq` is null: the request id this device's send went out under. */
    local: number | null
    /** Full address of who said it. */
    from: string
    body: string
    /** The message whose thread this is in, if any. */
    parent: number | null
    sentAtMs: number
    edited: boolean
    deleted: boolean
    /** Emoji → the addresses that reacted with it, in the order they did. */
    reactions: Record<string, string[]>
    /** An ActivityStreams Note with a Place, when the message is a point. */
    point: string | null
    picture: Picture | null
    /** Who took it down, when a moderator deleted it rather than its author. */
    removedBy?: string
}

/** One chat's history, oldest first, plus how far its reader has read. */
export interface Chat {
    messages: Message[]
    /** The highest `seq` the person has seen in this chat. */
    readSeq: number
}

/** Most messages kept per chat. Older ones fall off the front. */
export const MAX_KEPT = 5000

export function emptyChat(): Chat {
    return {messages: [], readSeq: 0}
}

function find(chat: Chat, seq: number): Message | undefined {
    // Newest first: almost every reaction, edit and reply is about something
    // recent.
    for (let i = chat.messages.length - 1; i >= 0; i--) {
        if (chat.messages[i].seq === seq) return chat.messages[i]
    }
    return undefined
}

function insert(chat: Chat, message: Message): void {
    // Kept in log order. A replay can deliver an entry this device already
    // holds (a reconnect from an older cursor), so a known seq is ignored.
    if (message.seq !== null && find(chat, message.seq)) return
    let i = chat.messages.length
    if (message.seq !== null) {
        while (i > 0) {
            const prev = chat.messages[i - 1]
            if (prev.seq !== null && prev.seq < message.seq) break
            if (prev.seq === null) {
                // A pending message of ours stays after everything delivered.
                i--
                continue
            }
            i--
        }
    }
    chat.messages.splice(i, 0, message)
    if (chat.messages.length > MAX_KEPT) chat.messages.splice(0, chat.messages.length - MAX_KEPT)
}

function react(chat: Chat, target: number, emoji: string, who: string, remove: boolean): void {
    const m = find(chat, target)
    if (!m) return
    const people = m.reactions[emoji] ?? []
    const at = people.indexOf(who)
    if (remove) {
        if (at >= 0) people.splice(at, 1)
    } else if (at < 0) {
        people.push(who)
    }
    if (people.length) {
        m.reactions[emoji] = people
    } else {
        delete m.reactions[emoji]
    }
}

/** A message this device is sending, shown before the server has placed it. */
export function pending(
    chat: Chat,
    local: number,
    from: string,
    body: string,
    parent: number | null,
    nowMs: number,
): Message {
    const m: Message = {
        body,
        deleted: false,
        edited: false,
        from,
        local,
        parent,
        picture: null,
        point: null,
        reactions: {},
        sentAtMs: nowMs,
        seq: null,
    }
    insert(chat, m)
    return m
}

/**
 * Apply one engine event to the chat it is about. Returns whether anything
 * changed. `me` is this person's full address: what this device does itself
 * (react, edit, delete) is applied by the caller as it is sent, since a device
 * never reads its own entries back.
 */
export function apply(chat: Chat, event: ChatEvent, me: string): boolean {
    switch (event.kind) {
        case 'line':
            insert(chat, {
                body: event.body,
                deleted: false,
                edited: false,
                from: event.from,
                local: null,
                parent: event.parent,
                picture: event.picture,
                point: event.point,
                reactions: {},
                sentAtMs: event.sent_at_ms,
                seq: event.seq,
            })
            return true
        case 'sent': {
            const m = chat.messages.find((x) => x.seq === null && x.local === event.request)
            if (!m) return false
            m.seq = event.seq
            m.local = null
            // Re-sorted: a line from somebody else may have taken an earlier
            // place in the log while this one was on its way.
            chat.messages.splice(chat.messages.indexOf(m), 1)
            insert(chat, m)
            if (m.from === me) chat.readSeq = Math.max(chat.readSeq, event.seq)
            return true
        }
        case 'reaction':
            react(chat, event.target, event.emoji, event.from, event.remove)
            return true
        case 'edited': {
            const m = find(chat, event.target)
            // Only the person who said something may change it.
            if (!m || m.from !== event.from || m.deleted) return false
            m.body = event.body
            m.edited = true
            return true
        }
        case 'deleted': {
            const m = find(chat, event.target)
            if (!m || m.from !== event.from) return false
            m.deleted = true
            m.body = ''
            m.reactions = {}
            m.point = null
            m.picture = null
            return true
        }
        default:
            return false
    }
}

/** This device's own reaction, applied as it is sent. */
export function reactLocally(chat: Chat, target: number, emoji: string, me: string, remove: boolean): void {
    react(chat, target, emoji, me, remove)
}

/** This device's own edit or delete, applied as it is sent. */
export function changeLocally(chat: Chat, target: number, me: string, body: string | null): boolean {
    const m = find(chat, target)
    if (!m || m.from !== me || m.deleted) return false
    if (body === null) {
        m.deleted = true
        m.body = ''
        m.reactions = {}
    } else {
        m.body = body
        m.edited = true
    }
    return true
}

/**
 * A community's moderator deleted the message at `seq` for everybody. Unlike
 * an author's delete this names nobody's own message, so it is not checked
 * against who sent it: the server only delivers it from a moderator.
 */
export function redactLocally(chat: Chat, seq: number, by: string): boolean {
    const m = find(chat, seq)
    if (!m || m.removedBy) return false
    m.deleted = true
    m.body = ''
    m.reactions = {}
    m.point = null
    m.picture = null
    m.removedBy = by
    return true
}

/** Messages in the chat itself, not in a thread. */
export function topLevel(chat: Chat): Message[] {
    return chat.messages.filter((m) => m.parent === null)
}

/** The thread under `parent`, oldest first. */
export function thread(chat: Chat, parent: number): Message[] {
    return chat.messages.filter((m) => m.parent === parent)
}

/** How many replies each top-level message has, by its seq. */
export function replyCounts(chat: Chat): Map<number, number> {
    const counts = new Map<number, number>()
    for (const m of chat.messages) {
        if (m.parent !== null && !m.deleted) counts.set(m.parent, (counts.get(m.parent) ?? 0) + 1)
    }
    return counts
}

/** Messages from other people the reader has not seen. */
export function unread(chat: Chat, me: string): number {
    let n = 0
    for (const m of chat.messages) {
        if (m.seq !== null && m.seq > chat.readSeq && m.from !== me && !m.deleted) n++
    }
    return n
}

/** Mark everything in the chat as seen. Returns whether that changed anything. */
export function markRead(chat: Chat): boolean {
    let last = chat.readSeq
    for (const m of chat.messages) if (m.seq !== null && m.seq > last) last = m.seq
    if (last === chat.readSeq) return false
    chat.readSeq = last
    return true
}

/** Whether `me` is named in `body` (`@name` or `@name@server`). */
export function mentions(body: string, me: string): boolean {
    const [name] = me.split('@')
    const re = new RegExp(`(^|\\s)@(${escape(name)}|${escape(me)})(?![\\w.-])`, 'i')
    return re.test(body)
}

function escape(s: string): string {
    return s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
}
