import m, {MithrilComponent} from '@garage44/mithril'
import {Button, Card, CloseButton, Dialog, FieldCheckbox, FieldText} from '@garage44/mithril/components'
import {required, validation, validations} from '@garage44/mithril/validation'

import {$s} from '@/app'
import {ChatClient, type Message, type WorkspaceView} from '@/lib/chat'
import {bind, local} from '@/lib/reactive'
import {ADMIN} from '@/lib/base'
import {playerSignIn, playerSignOut} from '@/lib/signup'

/**
 * Chat: the web UI as a work chat (`docs/mockups/workspace/`).
 *
 * Workspaces in the rail, their channels and the direct messages in the
 * sidebar, one chat in the middle, a thread beside it. Everything in it is end
 * to end encrypted; this page holds no keys and no history, only what the chat
 * worker (`src/worker/chat-worker.ts`) tells it.
 */

/** One client per page, made the first time the chat is shown. */
let client: ChatClient | null = null

function chat(): ChatClient {
    client ??= new ChatClient(() => m.redraw())
    return client
}

/** The reactions offered on a message, in this order. */
const QUICK = ['👍', '❤️', '😂', '🎉', '👀', '✅']

/** How someone is shown: the bare name for a neighbour, else the full address. */
function shown(address: string, me: string | null): string {
    const [name, server] = address.split('@')
    const home = me?.split('@')[1]
    return server && server === home ? name : address
}

function hue(s: string): number {
    let h = 0
    for (const c of s) h = (h * 31 + c.charCodeAt(0)) % 360
    return h
}

function Avatar(who: string, size = 32) {
    const name = who.split('@')[0]
    return (
        <span
            class="chat-avatar"
            style={`width:${size}px;height:${size}px;font-size:${Math.round(size * 0.42)}px;` +
                `background:hsl(${hue(name)} 30% 42%)`}
        >
            {name.slice(0, 1).toUpperCase()}
        </span>
    )
}

function time(ms: number): string {
    return new Date(ms).toLocaleTimeString([], {hour: '2-digit', minute: '2-digit'})
}

function day(ms: number): string {
    const d = new Date(ms)
    const today = new Date()
    if (d.toDateString() === today.toDateString()) return 'Today'
    const yesterday = new Date(today.getTime() - 86400000)
    if (d.toDateString() === yesterday.toDateString()) return 'Yesterday'
    return d.toLocaleDateString([], {day: 'numeric', month: 'long', weekday: 'long'})
}

/** Body text with links made clickable and mentions of anyone set apart. */
function Body(text: string, me: string | null) {
    const parts: m.Children[] = []
    const re = /(https?:\/\/[^\s<]+)|(@[\w.-]+(?:@[\w.:-]+)?)/g
    let last = 0
    for (let hit = re.exec(text); hit; hit = re.exec(text)) {
        if (hit.index > last) parts.push(text.slice(last, hit.index))
        if (hit[1]) {
            parts.push(<a href={hit[1]} rel="noopener noreferrer" target="_blank">{hit[1]}</a>)
        } else {
            const mine = !!me && (hit[2] === `@${me.split('@')[0]}` || hit[2] === `@${me}`)
            parts.push(<span class={mine ? 'chat-mention is-me' : 'chat-mention'}>{hit[2]}</span>)
        }
        last = hit.index + hit[0].length
    }
    if (last < text.length) parts.push(text.slice(last))
    return parts
}

interface PageState {
    /** Workspace id, or '' for direct messages and groups outside one. */
    workspace: string
    channel: string | null
    /** The message whose thread is open, by seq. */
    thread: number | null
    draft: Record<string, string>
    editing: {channel: string; seq: number; body: string} | null
    dialog: null | 'workspace' | 'channel' | 'browse' | 'direct' | 'people' | 'members'
    field: string
    field2: string
    privateChannel: boolean
    username: string
    password: string
    busy: boolean
}

export class Chat extends MithrilComponent {
    s = local<PageState>({
        busy: false,
        channel: null,
        dialog: null,
        draft: {},
        editing: null,
        field: '',
        field2: '',
        password: '',
        privateChannel: false,
        thread: null,
        username: '',
        workspace: '',
    })

    v = validations({
        password: validation(bind(this.s, 'password'), [required]),
        username: validation(bind(this.s, 'username'), [required]),
    })

    oncreate() {
        if ($s.signup.token) chat().signIn({token: $s.signup.token})
    }

    onupdate() {
        // Signed in on this page since the last draw: start the chat too.
        if ($s.signup.token && chat().status === 'signed_out') chat().signIn({token: $s.signup.token})
    }

    signIn = async (e: Event) => {
        e.preventDefault()
        const s = this.s
        if (s.busy || !this.v.$validate()) return
        s.busy = true
        const ok = await playerSignIn(s.username, s.password)
        s.busy = false
        if (ok) {
            s.password = ''
            this.v.$reset()
            chat().signIn({token: $s.signup.token})
        }
    }

    signOut = async () => {
        chat().signOut()
        await playerSignOut()
    }

    /** Open a chat; `null` shows nothing. */
    open(channel: string | null) {
        this.s.channel = channel
        this.s.thread = null
        this.s.editing = null
        chat().open(channel)
        askForNotifications()
    }

    view() {
        if (!$s.signup.token) return this.signInView()
        const c = chat()
        const s = this.s
        const ws = c.workspaces.find((w) => w.id === s.workspace) ?? null
        return (
            <div class="chat">
                {this.rail(c.workspaces)}
                {this.sidebar(ws)}
                <main class="chat-main">{s.channel ? this.channelView(s.channel, ws) : this.welcome(ws)}</main>
                {s.channel && s.thread !== null && this.threadView(s.channel, s.thread)}
                {s.dialog && this.dialogView(ws)}
            </div>
        )
    }

    /**
     * cartopolis.org for somebody not signed in: what the site is, the way into
     * the chat, and every other door the site has. It replaced the front page
     * (2026-10-09): the chat is what the site is for now, and the map, the app
     * and the operator's pages are reached from here and from the sidebar.
     */
    signInView() {
        const s = this.s
        return (
            <div class="chat-signin">
                {/* A real <form>, so the phone keyboard shows a Go key and Enter submits. */}
                <form class="auth chat-door" novalidate onsubmit={this.signIn}>
                    <Card>
                        <img alt="" class="chat-welcome-mark" src="/icon.svg" />
                        <div class="wordmark">carto<b>polis</b></div>
                        <p class="auth__lede">
                            Chat with your team, end-to-end encrypted, and meet them in a shared 3D world
                            on the real map.
                        </p>
                        <FieldText
                            autocomplete="username"
                            disabled={s.busy}
                            label="Username"
                            model={bind(s, 'username')}
                            validation={this.v.username}
                        />
                        <FieldText
                            autocomplete="current-password"
                            disabled={s.busy}
                            label="Password"
                            model={bind(s, 'password')}
                            type="password"
                            validation={this.v.password}
                        />
                        <div class="auth__actions">
                            <Button disabled={!this.v.$valid} loading={s.busy} type="submit" variant="primary">
                                Sign in
                            </Button>
                            <Button disabled={s.busy} onclick={() => m.route.set('/register')} variant="ghost">
                                Create an account
                            </Button>
                        </div>
                        <nav class="chat-options">
                            <a href="/viewer/">
                                <b>Open the map</b>
                                <span>The world in your browser, no account needed</span>
                            </a>
                            <a href="/apk/">
                                <b>Android app</b>
                                <span>Chat, the world and calls on your phone</span>
                            </a>
                            {Link('/buildings', <><b>Your buildings</b><span>Claim a building, draw its floors</span></>)}
                            {Link(ADMIN, <><b>Operators</b><span>Accounts, map data, the library</span></>)}
                        </nav>
                    </Card>
                </form>
            </div>
        )
    }

    rail(workspaces: WorkspaceView[]) {
        const s = this.s
        const c = chat()
        const dmUnread = c.chats
            .filter((x) => !channelOf(c.workspaces, x.channel))
            .reduce((n, x) => n + x.unread, 0)
        return (
            <nav class="chat-rail">
                <img alt="" class="chat-logo" src="/icon.svg" />
                <button
                    class={s.workspace === '' ? 'chat-ws is-current' : 'chat-ws'}
                    onclick={() => {
                        s.workspace = ''
                        this.open(null)
                    }}
                    title="Direct messages"
                    type="button"
                >
                    ✉
                    {dmUnread > 0 && <span class="chat-badge">{dmUnread}</span>}
                </button>
                {workspaces.map((w) => {
                    const n = w.channels.reduce(
                        (sum, ch) => sum + (c.chats.find((x) => x.channel === ch.channel)?.unread ?? 0), 0)
                    return (
                        <button
                            class={s.workspace === w.id ? 'chat-ws is-current' : 'chat-ws'}
                            key={w.id}
                            onclick={() => {
                                s.workspace = w.id
                                this.open(w.channels.find((ch) => ch.joined)?.channel ?? null)
                            }}
                            style={`background:hsl(${hue(w.name)} 28% 38%)`}
                            title={w.name}
                            type="button"
                        >
                            {w.name.split(/\s+/).map((p) => p[0]).join('').slice(0, 2).toUpperCase()}
                            {n > 0 && <span class="chat-badge">{n}</span>}
                        </button>
                    )
                })}
                <button
                    class="chat-ws is-add"
                    onclick={() => this.dialog('workspace')}
                    title="New workspace"
                    type="button"
                >
                    +
                </button>
                <span class="chat-rail-spacer" />
                {Avatar(c.address ?? $s.signup.username, 34)}
            </nav>
        )
    }

    dialog(d: PageState['dialog']) {
        this.s.dialog = d
        this.s.field = ''
        this.s.field2 = ''
        this.s.privateChannel = false
    }

    sidebar(ws: WorkspaceView | null) {
        const c = chat()
        const s = this.s
        const me = c.address
        const status = c.status === 'connected'
            ? null
            : c.status === 'waiting'
                ? 'Chat is open in another tab'
                : c.status === 'connecting'
                    ? 'Connecting…'
                    : 'Signed out of chat'

        const item = (channel: string, label: m.Children, unread: number, mentioned: boolean) => (
            <button
                class={[
                    'chat-item',
                    s.channel === channel ? 'is-current' : '',
                    unread > 0 ? 'is-unread' : '',
                ].join(' ')}
                key={channel}
                onclick={() => this.open(channel)}
                type="button"
            >
                {label}
                {unread > 0 && <span class={mentioned ? 'chat-count is-mention' : 'chat-count'}>{unread}</span>}
            </button>
        )

        if (!ws) {
            const outside = c.chats
                .filter((x) => !channelOf(c.workspaces, x.channel))
                .sort((a, b) => b.lastAtMs - a.lastAtMs)
            return (
                <aside class="chat-side">
                    <header class="chat-side-head">
                        <h2>Direct messages</h2>
                        <span class="chat-sub">{me ?? $s.signup.username}</span>
                    </header>
                    {status && <div class="chat-status">{status}</div>}
                    <section class="chat-section">
                        <h3>
                            Chats
                            <button class="chat-plus" onclick={() => this.dialog('direct')} title="New message" type="button">+</button>
                        </h3>
                        {outside.length === 0 && <p class="chat-empty">No chats yet.</p>}
                        {outside.map((x) => item(
                            x.channel,
                            <>
                                {x.group ? <span class="chat-hash">⧉</span> : Avatar(x.people[0] ?? x.label, 22)}
                                <span class="chat-label">{x.group ? x.label : shown(x.label, me)}</span>
                            </>,
                            x.unread,
                            x.mentioned,
                        ))}
                    </section>
                    {this.sideFooter()}
                </aside>
            )
        }

        const joined = ws.channels.filter((ch) => ch.joined)
        const others = ws.channels.filter((ch) => !ch.joined).length
        const admin = ws.role !== 'member'
        return (
            <aside class="chat-side">
                <header class="chat-side-head">
                    <h2>{ws.name}</h2>
                    <span class="chat-sub">{ws.members.length} {ws.members.length === 1 ? 'member' : 'members'} · {ws.home}</span>
                </header>
                {status && <div class="chat-status">{status}</div>}
                <section class="chat-section">
                    <h3>
                        Channels
                        <button class="chat-plus" onclick={() => this.dialog('channel')} title="New channel" type="button">+</button>
                    </h3>
                    {joined.map((ch) => {
                        const sum = c.chats.find((x) => x.channel === ch.channel)
                        return item(
                            ch.channel,
                            <>
                                <span class="chat-hash">{ch.private ? '🔒' : '#'}</span>
                                <span class="chat-label">{ch.name}</span>
                            </>,
                            sum?.unread ?? 0,
                            sum?.mentioned ?? false,
                        )
                    })}
                    {others > 0 && (
                        <button class="chat-item is-quiet" onclick={() => this.dialog('browse')} type="button">
                            <span class="chat-hash">⌕</span>
                            <span class="chat-label">Browse {others} more</span>
                        </button>
                    )}
                </section>
                <section class="chat-section">
                    <h3>
                        Members
                        {admin && <button class="chat-plus" onclick={() => this.dialog('people')} title="Add people" type="button">+</button>}
                    </h3>
                    {ws.members.map((p) => {
                        const direct = c.chats.find((x) => !x.group && x.label.toLowerCase() === p.address.toLowerCase())
                        return (
                            <button
                                class="chat-item"
                                key={p.address}
                                onclick={() => {
                                    if (p.address === me) return
                                    if (direct) {
                                        s.workspace = ''
                                        this.open(direct.channel)
                                    } else {
                                        c.startDirect(p.address)
                                        s.workspace = ''
                                    }
                                }}
                                type="button"
                            >
                                {Avatar(p.address, 22)}
                                <span class="chat-label">{shown(p.address, me)}{p.address === me ? ' (you)' : ''}</span>
                                {p.role !== 'member' && <span class="chat-role">{p.role}</span>}
                            </button>
                        )
                    })}
                </section>
                {this.sideFooter()}
            </aside>
        )
    }

    sideFooter() {
        const c = chat()
        return (
            <footer class="chat-side-foot">
                {c.notice && (
                    <div class="chat-notice" onclick={() => (c.notice = null)}>
                        {c.notice}
                    </div>
                )}
                <nav class="chat-side-links">
                    <a href="/viewer/">Map</a>
                    <a href="/apk/">App</a>
                    {Link('/buildings', 'Buildings')}
                </nav>
                <Button onclick={() => void this.signOut()} size="sm" variant="ghost">Sign out</Button>
            </footer>
        )
    }

    welcome(ws: WorkspaceView | null) {
        return (
            <div class="chat-welcome">
                <h1>{ws ? ws.name : 'Direct messages'}</h1>
                <p class="chat-hint">
                    {ws
                        ? 'Pick a channel, or start one. Everything said here is end-to-end encrypted: the server stores it sealed and cannot read it.'
                        : chat().workspaces.length
                            ? 'Pick a chat, or start one with the + beside Chats.'
                            : 'Start a workspace for your team with the + in the rail, or message somebody directly.'}
                </p>
            </div>
        )
    }

    channelView(channel: string, ws: WorkspaceView | null) {
        const c = chat()
        const s = this.s
        const me = c.address
        const sum = c.chats.find((x) => x.channel === channel)
        const meta = channelOf(c.workspaces, channel)
        const messages = c.histories.get(channel) ?? []
        const top = messages.filter((x) => x.parent === null)
        const replies = new Map<number, Message[]>()
        for (const x of messages) {
            if (x.parent !== null && !x.deleted) replies.set(x.parent, [...(replies.get(x.parent) ?? []), x])
        }
        const title = meta ? meta.name : sum ? (sum.group ? sum.label : shown(sum.label, me)) : '…'
        return (
            <>
                <header class="chat-head">
                    <Button class="chat-back" iconOnly onclick={() => this.open(null)} title="Back" variant="ghost">‹</Button>
                    <div class="chat-head-title">
                        <b>
                            {meta ? (meta.private ? '🔒 ' : '# ') : ''}
                            {title}
                        </b>
                        <span class="chat-sub">
                            <span class="chat-lock" title="End-to-end encrypted">🔐 encrypted</span>
                            {meta?.topic ? ` · ${meta.topic}` : ''}
                            {sum ? ` · ${sum.people.length + 1} people` : ''}
                        </span>
                    </div>
                    {sum?.group && (
                        <Button onclick={() => this.dialog('members')} size="sm" variant="ghost">People</Button>
                    )}
                </header>
                <div class="chat-messages" onupdate={stickToBottom} oncreate={stickToBottom}>
                    {top.length === 0 && <p class="chat-empty">Nothing said here yet.</p>}
                    {this.messageList(channel, top, replies, me)}
                </div>
                {this.composer(channel, null, meta ? `Message #${meta.name}` : `Message ${title}`)}
            </>
        )
    }

    messageList(channel: string, list: Message[], replies: Map<number, Message[]> | null, me: string | null) {
        const out: m.Children[] = []
        let lastDay = ''
        let prev: Message | null = null
        for (const msg of list) {
            const d = day(msg.sentAtMs)
            if (d !== lastDay) {
                out.push(<div class="chat-day" key={`day-${d}`}>{d}</div>)
                lastDay = d
                prev = null
            }
            // A run of lines from one person within five minutes reads as one.
            const continued = !!prev && prev.from === msg.from && msg.sentAtMs - prev.sentAtMs < 5 * 60000
            out.push(this.messageView(channel, msg, continued, replies?.get(msg.seq ?? -1) ?? [], me))
            prev = msg
        }
        return out
    }

    messageView(channel: string, msg: Message, continued: boolean, replies: Message[], me: string | null) {
        const c = chat()
        const s = this.s
        const mine = msg.from === me
        const key = msg.seq !== null ? `s${msg.seq}` : `l${msg.local}`
        const editing = s.editing && s.editing.channel === channel && s.editing.seq === msg.seq
        return (
            <div class={['chat-msg', continued ? 'is-continued' : '', msg.seq === null ? 'is-pending' : ''].join(' ')} key={key}>
                <div class="chat-msg-gutter">
                    {continued ? <span class="chat-msg-time">{time(msg.sentAtMs)}</span> : Avatar(msg.from, 34)}
                </div>
                <div class="chat-msg-body">
                    {!continued && (
                        <div class="chat-msg-head">
                            <b>{shown(msg.from, me)}</b>
                            <span class="chat-msg-time">{time(msg.sentAtMs)}</span>
                        </div>
                    )}
                    {msg.deleted ? (
                        <p class="chat-msg-text is-deleted">This message was deleted.</p>
                    ) : editing ? (
                        <form
                            class="chat-edit"
                            onsubmit={(e: Event) => {
                                e.preventDefault()
                                if (s.editing && s.editing.body.trim()) c.edit(channel, s.editing.seq, s.editing.body.trim())
                                s.editing = null
                            }}
                        >
                            <textarea
                                oncreate={(v: m.VnodeDOM) => (v.dom as HTMLTextAreaElement).focus()}
                                oninput={(e: Event) => {
                                    if (s.editing) s.editing.body = (e.target as HTMLTextAreaElement).value
                                }}
                                onkeydown={(e: KeyboardEvent) => {
                                    if (e.key === 'Escape') s.editing = null
                                    if (e.key === 'Enter' && !e.shiftKey) {
                                        e.preventDefault()
                                        ;(e.target as HTMLTextAreaElement).form?.requestSubmit()
                                    }
                                }}
                                value={s.editing?.body}
                            />
                            <span class="chat-hint">Enter to save · Esc to cancel</span>
                        </form>
                    ) : (
                        <p class="chat-msg-text">
                            {msg.picture ? <i>{msg.body || 'Picture'} (open it in the app)</i> : Body(msg.body, me)}
                            {msg.point && <span class="chat-point"> 📍 a place</span>}
                            {msg.edited && <span class="chat-edited"> (edited)</span>}
                        </p>
                    )}
                    {Object.keys(msg.reactions).length > 0 && (
                        <div class="chat-reactions">
                            {Object.entries(msg.reactions).map(([emoji, who]) => {
                                const reacted = !!me && who.includes(me)
                                return (
                                    <button
                                        class={reacted ? 'chat-reaction is-mine' : 'chat-reaction'}
                                        key={emoji}
                                        onclick={() => msg.seq !== null && c.react(channel, msg.seq, emoji, reacted)}
                                        title={who.map((w) => shown(w, me)).join(', ')}
                                        type="button"
                                    >
                                        {emoji} {who.length}
                                    </button>
                                )
                            })}
                        </div>
                    )}
                    {replies.length > 0 && msg.seq !== null && (
                        <button class="chat-replies" onclick={() => (s.thread = msg.seq)} type="button">
                            {replies.length} {replies.length === 1 ? 'reply' : 'replies'}
                            <span class="chat-sub"> · last {time(replies[replies.length - 1].sentAtMs)}</span>
                        </button>
                    )}
                </div>
                {msg.seq !== null && !msg.deleted && !editing && (
                    <div class="chat-actions">
                        {QUICK.slice(0, 3).map((emoji) => (
                            <button
                                key={emoji}
                                onclick={() => {
                                    const mineAlready = !!me && (msg.reactions[emoji] ?? []).includes(me)
                                    c.react(channel, msg.seq!, emoji, mineAlready)
                                }}
                                title={`React ${emoji}`}
                                type="button"
                            >
                                {emoji}
                            </button>
                        ))}
                        <details class="chat-more-emoji">
                            <summary title="More reactions">☺</summary>
                            <div>
                                {QUICK.slice(3).map((emoji) => (
                                    <button key={emoji} onclick={() => c.react(channel, msg.seq!, emoji, false)} type="button">
                                        {emoji}
                                    </button>
                                ))}
                            </div>
                        </details>
                        {msg.parent === null && (
                            <button onclick={() => (s.thread = msg.seq)} title="Reply in thread" type="button">💬</button>
                        )}
                        {mine && (
                            <button
                                onclick={() => (s.editing = {body: msg.body, channel, seq: msg.seq!})}
                                title="Edit"
                                type="button"
                            >
                                ✎
                            </button>
                        )}
                        {mine && (
                            <button
                                onclick={() => {
                                    if (confirm('Delete this message for everyone?')) c.remove(channel, msg.seq!)
                                }}
                                title="Delete"
                                type="button"
                            >
                                🗑
                            </button>
                        )}
                    </div>
                )}
            </div>
        )
    }

    threadView(channel: string, parent: number) {
        const c = chat()
        const me = c.address
        const messages = c.histories.get(channel) ?? []
        const root = messages.find((x) => x.seq === parent)
        const replies = messages.filter((x) => x.parent === parent)
        return (
            <aside class="chat-thread">
                <header class="chat-head">
                    <div class="chat-head-title">
                        <b>Thread</b>
                    </div>
                    <CloseButton label="Close thread" onclick={() => (this.s.thread = null)} />
                </header>
                <div class="chat-messages" onupdate={stickToBottom} oncreate={stickToBottom}>
                    {root && this.messageView(channel, root, false, [], me)}
                    <div class="chat-day">{replies.length} {replies.length === 1 ? 'reply' : 'replies'}</div>
                    {this.messageList(channel, replies, null, me)}
                </div>
                {this.composer(channel, parent, 'Reply…')}
            </aside>
        )
    }

    composer(channel: string, parent: number | null, placeholder: string) {
        const s = this.s
        const key = parent === null ? channel : `${channel}#${parent}`
        const send = () => {
            const body = (s.draft[key] ?? '').trim()
            if (!body) return
            chat().sendText(channel, body, parent)
            s.draft = {...s.draft, [key]: ''}
        }
        return (
            <form
                class="chat-composer"
                onsubmit={(e: Event) => {
                    e.preventDefault()
                    send()
                }}
            >
                <textarea
                    oninput={(e: Event) => (s.draft = {...s.draft, [key]: (e.target as HTMLTextAreaElement).value})}
                    onkeydown={(e: KeyboardEvent) => {
                        if (e.key === 'Enter' && !e.shiftKey) {
                            e.preventDefault()
                            send()
                        }
                    }}
                    placeholder={placeholder}
                    rows={1}
                    value={s.draft[key] ?? ''}
                />
                <div class="chat-composer-tools">
                    <span class="chat-sub">🔐 end-to-end encrypted · Enter to send, Shift+Enter for a new line</span>
                    <Button disabled={!(s.draft[key] ?? '').trim()} size="sm" type="submit" variant="primary">Send</Button>
                </div>
            </form>
        )
    }

    dialogView(ws: WorkspaceView | null) {
        const c = chat()
        const s = this.s
        const close = () => (s.dialog = null)
        // The library's dialog is open while its model is true; Escape, the
        // backdrop and its close button write false, which closes ours.
        const open = {
            get value() {
                return s.dialog !== null
            },
            set value(v: boolean) {
                if (!v) close()
            },
        }
        const input = (label: string, field: 'field' | 'field2', placeholder = '') => (
            <FieldText label={label} model={bind(s, field)} placeholder={placeholder} />
        )
        const form = (title: string, body: m.Children, submit: string, ok: () => void, valid = true) => {
            const go = () => {
                if (!valid) return
                ok()
                close()
            }
            return (
                <Dialog
                    class="chat-dialog"
                    footer={
                        <>
                            <Button onclick={close} variant="ghost">Cancel</Button>
                            <Button disabled={!valid} onclick={go} variant="primary">{submit}</Button>
                        </>
                    }
                    model={open}
                    title={title}
                >
                    {/* A form, so Enter in a field submits. */}
                    <form
                        class="chat-dialog-form"
                        novalidate
                        onsubmit={(e: Event) => {
                            e.preventDefault()
                            go()
                        }}
                    >
                        {body}
                        <button hidden type="submit" />
                    </form>
                </Dialog>
            )
        }
        switch (s.dialog) {
            case 'workspace':
                return form(
                    'New workspace',
                    <>
                        <p class="chat-hint">A workspace is a team: its people and its channels. You will be its owner.</p>
                        {input('Name', 'field', 'Atelier Noord')}
                    </>,
                    'Create',
                    () => c.createWorkspace(s.field),
                    !!s.field.trim(),
                )
            case 'channel':
                return ws && form(
                    `New channel in ${ws.name}`,
                    <>
                        {input('Name', 'field', 'ebbinge-quarter')}
                        {input('Topic', 'field2', 'What it is about (optional)')}
                        <FieldCheckbox
                            label="Private: only people you add can see it"
                            model={bind(s, 'privateChannel')}
                        />
                    </>,
                    'Create',
                    () => c.startChannel(ws.id, s.field, s.field2, s.privateChannel),
                    !!s.field.trim(),
                )
            case 'browse':
                return ws && (
                    <Dialog
                        class="chat-dialog"
                        footer={<Button onclick={close} variant="ghost">Close</Button>}
                        model={open}
                        title={`Channels in ${ws.name}`}
                    >
                        {ws.channels.filter((ch) => !ch.joined).map((ch) => (
                            <div class="chat-browse" key={ch.channel}>
                                <div>
                                    <b># {ch.name}</b>
                                    <span class="chat-sub"> · {ch.members} {ch.members === 1 ? 'member' : 'members'}</span>
                                    {ch.topic && <p class="chat-sub">{ch.topic}</p>}
                                </div>
                                {ch.pending ? (
                                    <span class="chat-sub">Waiting for a member to let you in…</span>
                                ) : (
                                    <Button onclick={() => c.joinChannel(ws.id, ch.channel)} size="sm" variant="primary">Join</Button>
                                )}
                            </div>
                        ))}
                    </Dialog>
                )
            case 'direct':
                return form(
                    'New message',
                    <>
                        <p class="chat-hint">A username on this server, or a full address. A first message to somebody who is not a contact yet arrives as a request.</p>
                        {input('To', 'field', 'sanne')}
                    </>,
                    'Start',
                    () => c.startDirect(s.field.trim()),
                    !!s.field.trim(),
                )
            case 'people':
                return ws && form(
                    `Add people to ${ws.name}`,
                    <>
                        <p class="chat-hint">Somebody with an account on {ws.home}. They will see its public channels and can join them.</p>
                        {input('Username', 'field', 'joris')}
                    </>,
                    'Add',
                    () => c.addWorkspaceMember(ws.id, s.field.trim()),
                    !!s.field.trim(),
                )
            case 'members': {
                const sum = c.chats.find((x) => x.channel === s.channel)
                return form(
                    'People in this chat',
                    <>
                        <ul class="chat-people">
                            {c.address && <li>{Avatar(c.address, 22)} {shown(c.address, c.address)} (you)</li>}
                            {(sum?.people ?? []).map((p) => <li key={p}>{Avatar(p, 22)} {shown(p, c.address)}</li>)}
                        </ul>
                        {input('Add somebody', 'field', 'username')}
                    </>,
                    'Add',
                    () => s.channel && c.addToChat(s.channel, s.field.trim()),
                    !!s.field.trim(),
                )
            }
        }
        return null
    }
}

/**
 * A link to another page of this app, through the router. The map and the APK
 * are plain links instead: `/viewer/` and `/apk/` are other builds Caddy serves,
 * and the router would swallow the click.
 */
function Link(href: string, children: m.Children) {
    return (
        <a
            href={href}
            onclick={(e: Event) => {
                e.preventDefault()
                m.route.set(href)
            }}
        >
            {children}
        </a>
    )
}

/** The workspace channel a chat is, if it is one. */
function channelOf(workspaces: WorkspaceView[], channel: string) {
    for (const w of workspaces) {
        const ch = w.channels.find((x) => x.channel === channel)
        if (ch) return ch
    }
    return null
}

/** Keep a message list scrolled to the newest line, unless the reader scrolled up. */
function stickToBottom(vnode: m.VnodeDOM) {
    const el = vnode.dom as HTMLElement & {_pinned?: boolean}
    if (el._pinned === undefined) {
        el._pinned = true
        el.addEventListener('scroll', () => {
            el._pinned = el.scrollHeight - el.scrollTop - el.clientHeight < 40
        })
    }
    if (el._pinned) el.scrollTop = el.scrollHeight
}

/** Ask once, on a click, for permission to show notifications. */
function askForNotifications() {
    if (typeof Notification !== 'undefined' && Notification.permission === 'default') {
        void Notification.requestPermission()
    }
}
