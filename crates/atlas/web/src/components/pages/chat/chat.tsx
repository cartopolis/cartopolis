import m, {MithrilComponent} from '@garage44/mithril'
import {Button, Card, CloseButton, Dialog, FieldCheckbox, FieldSelect, FieldText} from '@garage44/mithril/components'
import {required, validation, validations} from '@garage44/mithril/validation'

import {$s, signupApi} from '@/app'
import {
    ChatClient,
    type CommunityListing,
    type CommunityPlace,
    type CommunitySettings,
    type CommunityView,
    type Message,
    type Role,
} from '@/lib/chat'
import {bind, local} from '@/lib/reactive'
import {ADMIN} from '@/lib/base'
import {playerSignIn, playerSignOut} from '@/lib/signup'

/**
 * Chat: cartopolis.org's front door (`docs/mockups/communities/`).
 *
 * One list down the side — communities with their topics, then people and
 * groups — and one chat in the middle, with a thread beside it. A community is
 * a named set of people; its topics are encrypted chats that every member is
 * in. Everything said here is end-to-end encrypted; this page holds no keys
 * and no history, only what the chat worker (`src/worker/chat-worker.ts`)
 * tells it.
 */

/** One client per page, made the first time the chat is shown. */
let client: ChatClient | null = null

function chat(): ChatClient {
    client ??= new ChatClient(() => m.redraw())
    return client
}

/** The reactions offered on a message, in this order. */
const QUICK = ['👍', '❤️', '😂', '🎉', '👀', '✅']

/** The reasons a report can give, as the server names them. */
const REASONS = [
    {label: 'Spam or a scam', value: 'spam'},
    {label: 'Harassment or bullying', value: 'harassment'},
    {label: 'Hate speech or a slur', value: 'hate_speech'},
    {label: 'Sexual content', value: 'sexual_content'},
    {label: 'Threat or self-harm', value: 'threat'},
    {label: 'Something else', value: 'other'},
]

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

/**
 * A call's audio or video element, its stream set on the element itself
 * (`srcObject` is a property, not an attribute) whenever it changes.
 */
function Media(stream: MediaStream, kind: 'audio' | 'video', muted: boolean) {
    const attach = (v: m.VnodeDOM) => {
        const el = v.dom as HTMLMediaElement
        if (el.srcObject !== stream) el.srcObject = stream
    }
    return kind === 'video'
        ? <video autoplay muted={muted} oncreate={attach} onupdate={attach} playsinline />
        : <audio autoplay oncreate={attach} onupdate={attach} />
}

/** Community pictures as object URLs, by hash: fetched once, with the session. */
const pictures = new Map<string, string | 'loading' | 'failed'>()

function pictureUrl(hash: string | null | undefined): string | null {
    if (!hash) return null
    const have = pictures.get(hash)
    if (have === undefined) {
        pictures.set(hash, 'loading')
        void fetch(`/api/pictures/${hash}`, {headers: {Authorization: `Bearer ${$s.signup.token}`}})
            .then(async (r) => {
                if (!r.ok) throw new Error(String(r.status))
                pictures.set(hash, URL.createObjectURL(await r.blob()))
            })
            .catch(() => pictures.set(hash, 'failed'))
            .finally(() => m.redraw())
        return null
    }
    return have === 'loading' || have === 'failed' ? null : have
}

/** A community's tile: its picture, or a rounded square with its initial. */
function Tile(name: string, size = 32, picture: string | null = null) {
    const url = pictureUrl(picture)
    if (url) {
        return <img alt="" class="chat-avatar is-tile" src={url} style={`width:${size}px;height:${size}px;object-fit:cover`} />
    }
    return (
        <span
            class="chat-avatar is-tile"
            style={`width:${size}px;height:${size}px;font-size:${Math.round(size * 0.42)}px;` +
                `background:hsl(${hue(name)} 28% 38%)`}
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

const RULE: Record<string, string> = {ask: 'Asks first', invite: 'Invite only', open: 'Open'}
const ROLE: Record<Role, string> = {member: 'Member', moderator: 'Moderator', owner: 'Owner'}

function moderates(c: CommunityView | null | undefined): boolean {
    return c?.role === 'owner' || c?.role === 'moderator'
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

/** What the middle of the page shows. */
type Pane =
    | {kind: 'chat'; channel: string}
    | {kind: 'community'; id: string}
    | {kind: 'edit'; id: string}
    | {kind: 'find'}
    | {kind: 'preview'; id: string}
    | {kind: 'create'}
    | {code: string; kind: 'invite'}
    | {kind: 'welcome'}

interface Draft {
    about: string
    join: string
    listed: boolean
    members_start_topics: boolean
    name: string
    people: string
    picture: string | null
    place: CommunityPlace | null
    topics: string
    /** Editing: each topic's name and whether it is archived, by channel. */
    topicEdits: {archived: boolean; channel: string; name: string}[]
}

interface PageState {
    pane: Pane
    /** The message whose thread is open, by seq. */
    thread: number | null
    draft: Record<string, string>
    editing: {channel: string; seq: number; body: string} | null
    dialog: null | 'direct' | 'group' | 'members' | 'person' | 'report'
    field: string
    field2: string
    query: string
    order: string
    community: Draft
    /** The person menu's subject, and the topic it was opened from. */
    person: {address: string; channel: string | null}
    report: {channel: string; line: string; note: string; reason: string; seq: number; who: string}
    /** Whether this account may suspend and delete accounts on this server. */
    admin: boolean
    adminChecked: boolean
    username: string
    password: string
    busy: boolean
}

function emptyDraft(): Draft {
    return {
        about: '',
        join: 'ask',
        listed: true,
        members_start_topics: true,
        name: '',
        people: '',
        picture: null,
        place: null,
        topicEdits: [],
        topics: 'general',
    }
}

function settingsOf(d: Draft): CommunitySettings {
    return {
        about: d.about.trim(),
        join: d.join as CommunitySettings['join'],
        listed: d.listed,
        members_start_topics: d.members_start_topics,
        name: d.name.trim(),
        picture: d.picture,
        place: d.place && {...d.place, name: d.place.name.trim()},
    }
}

function list(s: string): string[] {
    return s.split(/[,\s]+/).map((x) => x.trim()).filter(Boolean)
}

export class Chat extends MithrilComponent {
    s = local<PageState>({
        admin: false,
        adminChecked: false,
        busy: false,
        community: emptyDraft(),
        dialog: null,
        draft: {},
        editing: null,
        field: '',
        field2: '',
        order: 'popular',
        pane: {kind: 'welcome'},
        password: '',
        person: {address: '', channel: null},
        query: '',
        report: {channel: '', line: '', note: '', reason: 'spam', seq: 0, who: ''},
        thread: null,
        username: '',
    })

    v = validations({
        password: validation(bind(this.s, 'password'), [required]),
        username: validation(bind(this.s, 'username'), [required]),
    })

    oncreate() {
        if ($s.signup.token) chat().signIn({token: $s.signup.token})
        // An invite link (`/go?community=…` on the viewer, or `?community=` here).
        const code = inviteCode(new URLSearchParams(location.search).get('community') ?? '')
        if (code) {
            history.replaceState(null, '', location.pathname)
            this.openInvite(code)
        }
    }

    onupdate() {
        // Signed in on this page since the last draw: start the chat too.
        if ($s.signup.token && chat().status === 'signed_out') chat().signIn({token: $s.signup.token})
        if ($s.signup.token && !this.s.adminChecked) void this.checkAdmin()
        this.askInvite()
    }

    /** Whether this account may manage accounts: the person menu then has a server section. */
    async checkAdmin() {
        this.s.adminChecked = true
        try {
            const me = await signupApi.get<{admin: boolean; permissions: string[]}>('/api/admin/me')
            this.s.admin = me.admin || me.permissions.includes('accounts_manage')
        } catch {
            this.s.admin = false
        }
        m.redraw()
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
        this.s.adminChecked = false
        this.s.admin = false
        await playerSignOut()
    }

    /** Open a chat; `null` shows the welcome. */
    open(channel: string | null) {
        this.s.pane = channel ? {kind: 'chat', channel} : {kind: 'welcome'}
        this.s.thread = null
        this.s.editing = null
        chat().open(channel)
        askForNotifications()
    }

    show(pane: Pane) {
        closeMenus()
        this.s.pane = pane
        this.s.thread = null
        chat().open(null)
    }

    find() {
        this.search()
        this.show({kind: 'find'})
    }

    /** Where this browser is, for Near you; `null` until it says (or if it won't). */
    near: [number, number] | null = null
    /** Something to say about the picture or the place being set. */
    formNote: string | null = null
    /** A code or link typed into Find. */
    inviteInput = ''

    search() {
        const c = chat()
        const s = this.s
        if (s.order === 'near' && !this.near) {
            this.locate((at) => c.findCommunities(s.query, 'near', at))
            return
        }
        c.findCommunities(s.query, s.order as 'near' | 'new' | 'popular', this.near)
    }

    /** Ask the browser where it is. */
    locate(then: (at: [number, number]) => void) {
        if (!navigator.geolocation) {
            this.formNote = 'This browser cannot tell where it is.'
            return
        }
        navigator.geolocation.getCurrentPosition(
            (p) => {
                this.near = [p.coords.latitude, p.coords.longitude]
                this.formNote = null
                then(this.near)
                m.redraw()
            },
            () => {
                this.formNote = 'The browser did not say where it is. Allow location for this site and try again.'
                m.redraw()
            },
            {maximumAge: 600_000, timeout: 15_000},
        )
    }

    /** Open the community an invite code names. */
    openInvite(code: string) {
        chat().notice = null
        chat().directory = []
        this.inviteAsked = null
        this.show({code, kind: 'invite'})
        this.askInvite()
    }

    /** The invite this page has looked up, so it is asked once the chat is connected. */
    inviteAsked: string | null = null

    askInvite() {
        const pane = this.s.pane
        if (pane.kind !== 'invite' || this.inviteAsked === pane.code || chat().status !== 'connected') return
        this.inviteAsked = pane.code
        chat().lookupInvite(pane.code)
    }

    /** A picked picture: shrunk to a square and stored. Public: it is the community's face. */
    async choosePicture(file: File, d: Draft) {
        this.formNote = 'Uploading the picture…'
        m.redraw()
        try {
            const bitmap = await createImageBitmap(file)
            const edge = 256
            const canvas = document.createElement('canvas')
            canvas.width = edge
            canvas.height = edge
            const side = Math.min(bitmap.width, bitmap.height)
            canvas.getContext('2d')!.drawImage(
                bitmap,
                (bitmap.width - side) / 2,
                (bitmap.height - side) / 2,
                side,
                side,
                0,
                0,
                edge,
                edge,
            )
            const blob = await new Promise<Blob>((ok, fail) =>
                canvas.toBlob((b) => (b ? ok(b) : fail(new Error('could not encode it'))), 'image/jpeg', 0.82))
            const {hash} = await signupApi.upload<{hash: string}>(
                '/api/pictures',
                await blob.arrayBuffer(),
                'application/octet-stream',
            )
            d.picture = hash
            this.formNote = null
        } catch (e) {
            this.formNote = `The picture could not be used: ${(e as Error).message}`
        }
        m.redraw()
    }

    view() {
        if (!$s.signup.token) return this.signInView()
        const s = this.s
        const pane = s.pane
        return (
            <div class={pane.kind === 'welcome' ? 'chat' : 'chat is-open'}>
                {this.sidebar()}
                <main class="chat-main">
                    {this.ringingBanner()}
                    {pane.kind === 'chat' && this.channelView(pane.channel)}
                    {pane.kind === 'community' && this.communityView(pane.id)}
                    {pane.kind === 'edit' && this.editView(pane.id)}
                    {pane.kind === 'find' && this.findView()}
                    {pane.kind === 'preview' && this.previewView(pane.id)}
                    {pane.kind === 'create' && this.createView()}
                    {pane.kind === 'invite' && this.inviteView(pane.code)}
                    {pane.kind === 'welcome' && this.welcome()}
                </main>
                {pane.kind === 'chat' && s.thread !== null && this.threadView(pane.channel, s.thread)}
                {s.dialog && this.dialogView()}
            </div>
        )
    }

    /**
     * cartopolis.org for somebody not signed in: what the site is, the way into
     * the chat, and every other door the site has.
     */
    signInView() {
        const s = this.s
        return (
            <div class="chat-signin">
                {/* A real <form>, so the phone keyboard shows a Go key and Enter submits. */}
                <form class="auth chat-door" novalidate onsubmit={this.signIn}>
                    <Card>
                        <img alt="" class="chat-welcome-mark mark-dark" src="/icon.svg" />
                        <img alt="" class="chat-welcome-mark mark-light" src="/icon-light.svg" />
                        <div class="wordmark">carto<b>polis</b></div>
                        <p class="auth__lede">
                            Chat with your communities and your friends, end-to-end encrypted, and meet them
                            in a shared 3D world on the real map.
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

    /** The list: communities with their topics, then people and groups. */
    sidebar() {
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
        const current = s.pane.kind === 'chat' ? s.pane.channel : null
        const shownCommunity = s.pane.kind === 'community' || s.pane.kind === 'edit' ? s.pane.id : null
        const item = (channel: string, label: m.Children, unread: number, mentioned: boolean, indent = false) => (
            <button
                class={[
                    'chat-item',
                    current === channel ? 'is-current' : '',
                    unread > 0 ? 'is-unread' : '',
                    indent ? 'is-topic' : '',
                ].join(' ')}
                key={channel}
                onclick={() => this.open(channel)}
                type="button"
            >
                {label}
                {unread > 0 && <span class={mentioned ? 'chat-count is-mention' : 'chat-count'}>{unread}</span>}
            </button>
        )
        const outside = c.chats
            .filter((x) => !c.topicOf(x.channel))
            .sort((a, b) => b.lastAtMs - a.lastAtMs)
        return (
            <aside class="chat-side">
                <header class="chat-side-head">
                    <h2>Chats</h2>
                    <span class="chat-head-tools">
                        <button class="chat-plus" onclick={() => this.find()} title="Find a community" type="button">⌕</button>
                        <details class="chat-new">
                            <summary class="chat-plus" title="New">+</summary>
                            <div class="chat-new-menu">
                                <button onclick={() => this.dialog('direct')} type="button">
                                    <b>Message a person</b><span>Just the two of you</span>
                                </button>
                                <button onclick={() => this.dialog('group')} type="button">
                                    <b>Start a group</b><span>A few people you pick. Private.</span>
                                </button>
                                <button onclick={() => this.find()} type="button">
                                    <b>Find a community</b><span>Clubs, teams and neighbourhoods here</span>
                                </button>
                                <button
                                    onclick={() => {
                                        s.community = emptyDraft()
                                        this.show({kind: 'create'})
                                    }}
                                    type="button"
                                >
                                    <b>Start a community</b><span>People who share several topics</span>
                                </button>
                            </div>
                        </details>
                    </span>
                </header>
                {status && <div class="chat-status">{status}</div>}
                {c.communities.length > 0 && (
                    <section class="chat-section">
                        <h3>Communities</h3>
                        {c.communities.map((community) => {
                            const unread = community.topics.filter((t) => !c.isMuted(t.channel)).reduce(
                                (n, t) => n + (c.chats.find((x) => x.channel === t.channel)?.unread ?? 0), 0)
                            const asks = community.requests.length + community.reports.length
                            return (
                                <div class="chat-community" key={community.id}>
                                    <button
                                        class={shownCommunity === community.id ? 'chat-item is-current' : 'chat-item'}
                                        onclick={() => this.show({id: community.id, kind: 'community'})}
                                        type="button"
                                    >
                                        {Tile(community.settings.name, 22, community.settings.picture)}
                                        <span class="chat-label">{community.settings.name}</span>
                                        {community.role === null && <span class="chat-role">waiting</span>}
                                        {asks > 0 && <span class="chat-count is-mention">{asks}</span>}
                                        {asks === 0 && unread > 0 && <span class="chat-count">{unread}</span>}
                                    </button>
                                    {community.topics.filter((t) => t.joined && !t.archived).map((t) => {
                                        const sum = c.chats.find((x) => x.channel === t.channel)
                                        const muted = c.isMuted(t.channel)
                                        return item(
                                            t.channel,
                                            <>
                                                <span class="chat-hash">#</span>
                                                <span class="chat-label">{t.name}</span>
                                                {muted && <span class="chat-role">muted</span>}
                                            </>,
                                            muted ? 0 : sum?.unread ?? 0,
                                            sum?.mentioned ?? false,
                                            true,
                                        )
                                    })}
                                </div>
                            )
                        })}
                    </section>
                )}
                <section class="chat-section">
                    <h3>People and groups</h3>
                    {outside.length === 0 && <p class="chat-empty">No chats yet. Start one with the +.</p>}
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

    dialog(d: PageState['dialog']) {
        this.s.dialog = d
        this.s.field = ''
        this.s.field2 = ''
        closeMenus()
    }

    /** The menu behind a person's name: message, and what this account's roles allow. */
    person(address: string, channel: string | null) {
        this.s.person = {address, channel}
        this.dialog('person')
    }

    /** Report a line in a topic to its community's moderators. */
    report(channel: string, msg: Message) {
        this.s.report = {channel, line: msg.body, note: '', reason: 'spam', seq: msg.seq ?? 0, who: msg.from}
        this.dialog('report')
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
                {c.calls.notice && (
                    <div class="chat-notice" onclick={() => (c.calls.notice = null)}>
                        {c.calls.notice}
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

    welcome() {
        return (
            <div class="chat-welcome">
                <h1>Chats</h1>
                <p class="chat-hint">
                    {chat().communities.length
                        ? 'Pick a topic or a chat on the left, or start one with the +.'
                        : 'Find a community with ⌕, start one, or message somebody directly with the +. Everything said here is end-to-end encrypted: the server stores it sealed and cannot read it.'}
                </p>
            </div>
        )
    }

    /** A pane's header: back (on a phone), a title, and a line under it. */
    head(title: m.Children, sub: m.Children, tools: m.Children = null) {
        return (
            <header class="chat-head">
                <Button class="chat-back" iconOnly onclick={() => this.open(null)} title="Back" variant="ghost">‹</Button>
                <div class="chat-head-title">
                    <b>{title}</b>
                    <span class="chat-sub">{sub}</span>
                </div>
                {tools}
            </header>
        )
    }

    /** A private chat's call buttons: call (voice or video), join one going on, or hang up. */
    callButtons(channel: string) {
        const calls = chat().calls
        if (calls.active?.channel === channel) {
            return <Button onclick={() => calls.hangUp()} size="sm" variant="danger">Hang up</Button>
        }
        const others = calls.others(channel)
        return (
            <>
                <Button onclick={() => void calls.join(channel)} size="sm" variant={others.length ? 'primary' : 'ghost'}>
                    {others.length ? `Join call (${others.length})` : '📞 Call'}
                </Button>
                <Button onclick={() => void calls.join(channel, true)} size="sm" variant="ghost">🎥 Video</Button>
            </>
        )
    }

    /** The call this page is in: everybody's video, this device's own, and the controls. */
    callPanel() {
        const calls = chat().calls
        const a = calls.active
        if (!a) return null
        const others = calls.others(a.channel)
        const secs = a.since === null ? 0 : Math.floor((Date.now() - a.since) / 1000)
        const status = !a.connected
            ? 'Connecting…'
            : a.since === null
                ? 'Ringing…'
                : `${Math.floor(secs / 60)}:${String(secs % 60).padStart(2, '0')}`
        const self = calls.selfView()
        return (
            <section class="chat-call">
                <div class="chat-call-tiles">
                    {[...a.tiles.values()].map((t) => {
                        const who = others.find((o) => o.participant === t.participant)?.username ?? '…'
                        return (
                            <figure class="chat-call-tile">
                                {t.video ? Media(t.video, 'video', true) : Avatar(who, 64)}
                                {t.audio && Media(t.audio, 'audio', false)}
                                <figcaption>{who}</figcaption>
                            </figure>
                        )
                    })}
                    {a.tiles.size === 0 && (
                        <p class="chat-hint">{others.length ? 'Waiting for their picture and sound…' : 'Waiting for somebody to answer…'}</p>
                    )}
                    {self && (
                        <figure class="chat-call-tile is-self">
                            {Media(self, 'video', true)}
                            <figcaption>You</figcaption>
                        </figure>
                    )}
                </div>
                <div class="chat-call-bar">
                    <span class="chat-sub">🔐 {status}</span>
                    <Button onclick={() => calls.setMuted(!a.muted)} size="sm" variant="ghost">
                        {a.muted ? 'Unmute' : 'Mute'}
                    </Button>
                    <Button onclick={() => void calls.setCamera(!a.camera)} size="sm" variant="ghost">
                        {a.camera ? 'Camera off' : 'Camera on'}
                    </Button>
                    <Button onclick={() => calls.hangUp()} size="sm" variant="danger">Hang up</Button>
                </div>
            </section>
        )
    }

    /** A call ringing in one of this person's chats, wherever they are on the page. */
    ringingBanner() {
        const c = chat()
        const channel = c.calls.ringing()
        if (!channel) return null
        const caller = c.calls.others(channel)[0]?.username ?? 'Somebody'
        const topic = c.topicOf(channel)
        const sum = c.chats.find((x) => x.channel === channel)
        const where = topic ? `# ${topic.topic.name}` : sum?.group ? sum.label : null
        return (
            <div class="chat-ringing">
                <span>📞 <b>{caller}</b> is calling{where ? ` in ${where}` : ''}</span>
                <Button
                    onclick={() => {
                        this.open(channel)
                        void c.calls.join(channel)
                    }}
                    size="sm"
                    variant="primary"
                >
                    Answer
                </Button>
                <Button
                    onclick={() => {
                        this.open(channel)
                        void c.calls.join(channel, true)
                    }}
                    size="sm"
                    variant="ghost"
                >
                    With video
                </Button>
                <Button onclick={() => c.calls.decline(channel)} size="sm" variant="ghost">Decline</Button>
            </div>
        )
    }

    channelView(channel: string) {
        const c = chat()
        const me = c.address
        const sum = c.chats.find((x) => x.channel === channel)
        const topic = c.topicOf(channel)
        const messages = c.histories.get(channel) ?? []
        const top = messages.filter((x) => x.parent === null)
        const replies = new Map<number, Message[]>()
        for (const x of messages) {
            if (x.parent !== null && !x.deleted) replies.set(x.parent, [...(replies.get(x.parent) ?? []), x])
        }
        const title = topic
            ? `# ${topic.topic.name}`
            : sum ? (sum.group ? sum.label : shown(sum.label, me)) : '…'
        const sub = topic
            ? (
                <>
                    <a
                        class="chat-sub-link"
                        onclick={() => this.show({id: topic.community.id, kind: 'community'})}
                    >
                        {topic.community.settings.name}
                    </a>
                    {` · ${topic.community.members.length} people · 🔐 encrypted`}
                    {topic.topic.archived ? ' · archived' : ''}
                </>
            )
            : (
                <>
                    <span class="chat-lock" title="End-to-end encrypted">🔐 encrypted</span>
                    {sum ? ` · ${sum.people.length + 1} people` : ''}
                </>
            )
        return (
            <>
                {this.head(
                    title,
                    sub,
                    <>
                        {this.callButtons(channel)}
                        {!topic && sum?.group && (
                            <Button onclick={() => this.dialog('members')} size="sm" variant="ghost">People</Button>
                        )}
                    </>,
                )}
                {c.calls.active?.channel === channel && this.callPanel()}
                <div class="chat-messages" onupdate={stickToBottom} oncreate={stickToBottom}>
                    {top.length === 0 && <p class="chat-empty">Nothing said here yet.</p>}
                    {this.messageList(channel, top, replies, me)}
                </div>
                {topic?.topic.archived
                    ? <p class="chat-empty">This topic is archived: its history stays, and it takes no new messages.</p>
                    : this.composer(channel, null, topic ? `Message # ${topic.topic.name}` : `Message ${title}`)}
            </>
        )
    }

    findView() {
        const c = chat()
        const s = this.s
        const search = () => this.search()
        return (
            <>
                {this.head('Find a community', 'Every listed community on this server')}
                <div class="chat-pane">
                    <form
                        class="chat-find"
                        onsubmit={(e: Event) => {
                            e.preventDefault()
                            search()
                        }}
                    >
                        <FieldText label="Search" model={bind(s, 'query')} placeholder="Names and descriptions" />
                        <FieldSelect
                            label="Order"
                            model={{
                                get value() {
                                    return s.order
                                },
                                set value(v: string) {
                                    s.order = v ?? 'popular'
                                    search()
                                },
                            }}
                            options={[
                                {label: 'Popular', value: 'popular'},
                                {label: 'Near you', value: 'near'},
                                {label: 'New', value: 'new'},
                            ]}
                        />
                        <Button type="submit" variant="primary">Search</Button>
                    </form>
                    {s.order === 'near' && (
                        <p class="chat-hint">{this.formNote ?? 'Communities pinned to a place, nearest to you first.'}</p>
                    )}
                    {c.directory.length === 0 && (
                        <p class="chat-empty">
                            No communities found. Only communities that chose to be listed appear here.
                        </p>
                    )}
                    {c.directory.map((l) => (
                        <button
                            class="chat-card"
                            key={l.id}
                            onclick={() => this.show(l.mine === 'member' ? {id: l.id, kind: 'community'} : {id: l.id, kind: 'preview'})}
                            type="button"
                        >
                            {Tile(l.name, 40, l.picture)}
                            <span class="chat-card-text">
                                <b>{l.name}</b>
                                {l.distance_m !== null && <span class="chat-sub">{distanceWords(l.distance_m)}</span>}
                                <span class="chat-sub">{l.about || 'No description'}</span>
                                <span class="chat-sub">
                                    {l.members} {l.members === 1 ? 'person' : 'people'} · {listingState(l)}
                                </span>
                            </span>
                        </button>
                    ))}
                    <h3 class="chat-pane-h">Have an invite?</h3>
                    <form
                        class="chat-inline"
                        onsubmit={(e: Event) => {
                            e.preventDefault()
                            const code = inviteCode(this.inviteInput)
                            if (code) {
                                this.inviteInput = ''
                                this.openInvite(code)
                            }
                        }}
                    >
                        <FieldText label="The link or the code" model={bind(this, 'inviteInput')} />
                        <Button disabled={!inviteCode(this.inviteInput)} type="submit" variant="ghost">Open</Button>
                    </form>
                </div>
            </>
        )
    }

    /** Somebody opened an invite: the community it names, and Join. */
    inviteView(code: string) {
        const c = chat()
        const l = c.directory[0]
        if (!l) {
            return (
                <>
                    {this.head('Invitation', '')}
                    <div class="chat-pane">
                        <p class="chat-hint">
                            {c.notice ? 'This invite does not work any more. Ask whoever sent it for a new one.' : 'Looking the invite up…'}
                        </p>
                    </div>
                </>
            )
        }
        return (
            <>
                {this.head('Invitation', `${l.members} people · ${RULE[l.join]}`)}
                <div class="chat-pane">
                    {this.hero(l.name, l.picture, l.about, l.place)}
                    <div class="chat-join">
                        {l.mine === 'member'
                            ? <Button onclick={() => this.show({id: l.id, kind: 'community'})} variant="primary">Open</Button>
                            : (
                                <>
                                    <p class="chat-hint">You were invited, so you join straight away.</p>
                                    <Button
                                        onclick={() => {
                                            c.joinInvite(code)
                                            this.show({id: l.id, kind: 'community'})
                                        }}
                                        variant="primary"
                                    >
                                        Join
                                    </Button>
                                </>
                            )}
                    </div>
                </div>
            </>
        )
    }

    hero(name: string, picture: string | null, about: string, place: CommunityPlace | null) {
        return (
            <div class="chat-hero">
                {Tile(name, 64, picture)}
                <h2>{name}</h2>
                {about && <p class="chat-hint">{about}</p>}
                {place && <p class="chat-hint">📍 {placeWords(place)}</p>}
            </div>
        )
    }

    previewView(id: string) {
        const c = chat()
        const s = this.s
        const me = c.address
        const l = c.directory.find((x) => x.id === id)
        if (!l) return this.head('Community', 'No longer listed')
        return (
            <>
                {this.head(l.name, `${l.members} people · ${RULE[l.join]}`)}
                <div class="chat-pane">
                    {this.hero(l.name, l.picture, l.about, l.place)}
                    {l.topics.length > 0 && (
                        <p class="chat-topics">{l.topics.map((t) => <span key={t}># {t}</span>)}</p>
                    )}
                    <h3 class="chat-pane-h">Moderators</h3>
                    {l.moderators.map((p) => (
                        <div class="chat-person" key={p.address}>
                            {Avatar(p.address, 28)}
                            <span class="chat-label">{shown(p.address, me)}</span>
                            <span class="chat-role">{ROLE[p.role]}</span>
                        </div>
                    ))}
                    <div class="chat-join">
                        {l.mine === 'asked' && (
                            <>
                                <p class="chat-hint">You asked to join. A moderator will answer.</p>
                                <Button onclick={() => c.leaveCommunity(id)} variant="ghost">Withdraw the request</Button>
                            </>
                        )}
                        {l.mine === 'refused' && <p class="chat-hint">You can't ask to join this community again yet.</p>}
                        {l.mine === 'outside' && l.join === 'invite' && (
                            <p class="chat-hint">Only people a moderator adds can join this community.</p>
                        )}
                        {l.mine === 'outside' && l.join === 'open' && (
                            <Button
                                onclick={() => {
                                    c.joinCommunity(id, '')
                                    this.show({id, kind: 'community'})
                                }}
                                variant="primary"
                            >
                                Join
                            </Button>
                        )}
                        {l.mine === 'outside' && l.join === 'ask' && (
                            <form
                                onsubmit={(e: Event) => {
                                    e.preventDefault()
                                    c.joinCommunity(id, s.field.trim())
                                    s.field = ''
                                    this.show({kind: 'welcome'})
                                }}
                            >
                                <p class="chat-hint">
                                    A moderator confirms new members. Say who you are; they see your name and this
                                    message, nothing else.
                                </p>
                                <FieldText label="A line to the moderators (optional)" model={bind(s, 'field')} />
                                <Button type="submit" variant="primary">Ask to join</Button>
                            </form>
                        )}
                    </div>
                </div>
            </>
        )
    }

    settingsForm(d: Draft) {
        return (
            <>
                <FieldText label="Name" model={bind(d, 'name')} placeholder="Garage 44" />
                <FieldText label="What is it for? (optional)" model={bind(d, 'about')} placeholder="Building cartopolis together" />
                <FieldSelect
                    label="Who can join"
                    model={bind(d, 'join')}
                    options={[
                        {label: 'Anyone: joins straight away', value: 'open'},
                        {label: 'Anyone, after a moderator confirms', value: 'ask'},
                        {label: 'Only people a moderator adds', value: 'invite'},
                    ]}
                />
                <FieldCheckbox label="Show in Find a community (its name, description and size are public)" model={bind(d, 'listed')} />
                <FieldCheckbox label="Members may start topics" model={bind(d, 'members_start_topics')} />
                <h3 class="chat-pane-h">Picture</h3>
                <div class="chat-inline">
                    {Tile(d.name || '?', 48, d.picture)}
                    <label class="chat-file">
                        Choose a picture
                        <input
                            accept="image/*"
                            onchange={(e: Event) => {
                                const file = (e.target as HTMLInputElement).files?.[0]
                                if (file) void this.choosePicture(file, d)
                            }}
                            type="file"
                        />
                    </label>
                    {d.picture && (
                        <Button
                            onclick={(e: Event) => {
                                e.preventDefault()
                                d.picture = null
                            }}
                            size="sm"
                            variant="ghost"
                        >
                            Remove
                        </Button>
                    )}
                </div>
                <h3 class="chat-pane-h">Place</h3>
                {d.place
                    ? (
                        <div class="chat-inline">
                            <FieldText label="📍 Name the place" model={bind(d.place, 'name')} placeholder="Grote Markt, Groningen" />
                            <Button
                                onclick={(e: Event) => {
                                    e.preventDefault()
                                    d.place = null
                                }}
                                size="sm"
                                variant="ghost"
                            >
                                Remove
                            </Button>
                        </div>
                    )
                    : (
                        <Button
                            onclick={(e: Event) => {
                                e.preventDefault()
                                this.locate(([latitude, longitude]) => {
                                    d.place = {latitude, longitude, name: ''}
                                })
                            }}
                            size="sm"
                            variant="ghost"
                        >
                            📍 Pin it where I am
                        </Button>
                    )}
                <p class="chat-hint">
                    {this.formNote ?? (d.place
                        ? `Pinned at ${d.place.latitude.toFixed(4)}, ${d.place.longitude.toFixed(4)}. Near you lists it for people around here.`
                        : 'A community about somewhere can be pinned there, so people nearby find it.')}
                </p>
            </>
        )
    }

    createView() {
        const c = chat()
        const s = this.s
        const d = s.community
        return (
            <>
                {this.head('New community', 'People who share several topics, like a club or a team')}
                <form
                    class="chat-pane chat-form"
                    onsubmit={(e: Event) => {
                        e.preventDefault()
                        if (!d.name.trim()) return
                        c.createCommunity(settingsOf(d), d.topics.split(',').map((t) => t.trim()).filter(Boolean), list(d.people))
                        this.show({kind: 'welcome'})
                    }}
                >
                    {this.settingsForm(d)}
                    <FieldText label="Topics to start with" model={bind(d, 'topics')} placeholder="general, meet-ups" />
                    <FieldText label="People (optional)" model={bind(d, 'people')} placeholder="Usernames, separated by commas" />
                    <Button disabled={!d.name.trim()} type="submit" variant="primary">Start the community</Button>
                    <p class="chat-hint">What is said in a community is end-to-end encrypted, like every private chat.</p>
                </form>
            </>
        )
    }

    communityView(id: string) {
        const c = chat()
        const s = this.s
        const me = c.address
        const community = c.communities.find((x) => x.id === id)
        if (!community) return this.head('Community', 'You are no longer in it')
        const {settings} = community
        if (community.role === null) {
            return (
                <>
                    {this.head(settings.name, 'Waiting for a moderator')}
                    <div class="chat-pane">
                        <p class="chat-hint">You asked to join. A moderator will answer.</p>
                        <Button onclick={() => c.leaveCommunity(id)} variant="ghost">Withdraw the request</Button>
                    </div>
                </>
            )
        }
        const mod = moderates(community)
        return (
            <>
                {this.head(
                    settings.name,
                    `${community.members.length} people · ${RULE[settings.join]} · ${settings.listed ? 'listed' : 'unlisted'}`,
                    mod && (
                        <Button
                            onclick={() => {
                                s.community = {
                                    ...emptyDraft(),
                                    ...settings,
                                    topicEdits: community.topics.map((t) => ({archived: t.archived, channel: t.channel, name: t.name})),
                                }
                                this.show({id, kind: 'edit'})
                            }}
                            size="sm"
                            variant="ghost"
                        >
                            Edit
                        </Button>
                    ),
                )}
                <div class="chat-pane">
                    {this.hero(settings.name, settings.picture, settings.about, settings.place)}
                    {mod && community.requests.length > 0 && (
                        <>
                            <h3 class="chat-pane-h">Asking to join ({community.requests.length})</h3>
                            {community.requests.map((r) => (
                                <div class="chat-person" key={r.address}>
                                    {Avatar(r.address, 28)}
                                    <span class="chat-label">
                                        {shown(r.address, me)}
                                        <span class="chat-sub">{r.message ? ` “${r.message}”` : ' · no message'}</span>
                                    </span>
                                    <Button onclick={() => c.decideRequest(id, r.address, false)} size="sm" variant="ghost">Turn down</Button>
                                    <Button onclick={() => c.decideRequest(id, r.address, true)} size="sm" variant="primary">Let in</Button>
                                </div>
                            ))}
                            <p class="chat-hint">Letting someone in adds them to every topic. A refusal stands for a week.</p>
                        </>
                    )}
                    {mod && community.reports.length > 0 && (
                        <>
                            <h3 class="chat-pane-h">Reports ({community.reports.length})</h3>
                            {community.reports.map((r) => (
                                <div class="chat-report" key={r.id}>
                                    <span class="chat-sub">
                                        {shown(r.reporter, me)} reported {shown(r.reported, me)} · {REASONS.find((x) => x.value === r.reason)?.label ?? r.reason}
                                    </span>
                                    {r.line && <p>“{r.line}”</p>}
                                    {r.note && <p class="chat-hint">{r.note}</p>}
                                    <div class="chat-report-actions">
                                        <Button
                                            onclick={() => {
                                                c.moderateDelete(r.channel, r.seq)
                                                c.resolveReport(id, r.id)
                                            }}
                                            size="sm"
                                            variant="ghost"
                                        >
                                            Delete the message
                                        </Button>
                                        <Button
                                            onclick={() => {
                                                c.removeFromCommunity(id, r.reported, '', false)
                                                c.resolveReport(id, r.id)
                                            }}
                                            size="sm"
                                            variant="ghost"
                                        >
                                            Remove them
                                        </Button>
                                        <Button onclick={() => c.resolveReport(id, r.id)} size="sm" variant="ghost">Dismiss</Button>
                                    </div>
                                </div>
                            ))}
                        </>
                    )}
                    <h3 class="chat-pane-h">Topics</h3>
                    {community.topics.map((t) => (
                        <div class="chat-topic-row" key={t.channel}>
                            <button
                                class="chat-card"
                                disabled={!t.joined}
                                onclick={() => t.joined && this.open(t.channel)}
                                type="button"
                            >
                            <span class="chat-hash is-big">#</span>
                            <span class="chat-card-text">
                                <b>{t.name}</b>
                                <span class="chat-sub">
                                    {t.archived ? 'Archived: its history stays, nothing new' : t.joined ? 'Open' : 'Being added…'}
                                </span>
                            </span>
                            </button>
                            {t.joined && (
                                <Button
                                    onclick={() => c.setMuted(t.channel, !c.isMuted(t.channel))}
                                    size="sm"
                                    variant="ghost"
                                >
                                    {c.isMuted(t.channel) ? 'Unmute' : 'Mute'}
                                </Button>
                            )}
                        </div>
                    ))}
                    {(mod || settings.members_start_topics) && (
                        <form
                            class="chat-inline"
                            onsubmit={(e: Event) => {
                                e.preventDefault()
                                if (s.field2.trim()) c.startTopic(id, s.field2.trim())
                                s.field2 = ''
                            }}
                        >
                            <FieldText label="New topic" model={bind(s, 'field2')} placeholder="meet-ups" />
                            <Button disabled={!s.field2.trim()} type="submit" variant="ghost">Start topic</Button>
                        </form>
                    )}
                    <p class="chat-hint">Everyone here is in every topic. Somebody added later is given the last 300 messages of each by whoever adds them.</p>
                    <h3 class="chat-pane-h">People ({community.members.length})</h3>
                    {community.members.map((p) => (
                        <button
                            class="chat-person"
                            key={p.address}
                            onclick={() => p.address !== me && this.person(p.address, null)}
                            type="button"
                        >
                            {Avatar(p.address, 28)}
                            <span class="chat-label">{shown(p.address, me)}{p.address === me ? ' (you)' : ''}</span>
                            {p.role !== 'member' && <span class="chat-role">{ROLE[p.role]}</span>}
                        </button>
                    ))}
                    {mod && (
                        <form
                            class="chat-inline"
                            onsubmit={(e: Event) => {
                                e.preventDefault()
                                for (const who of list(s.field)) c.addToCommunity(id, who)
                                s.field = ''
                            }}
                        >
                            <FieldText label="Add people" model={bind(s, 'field')} placeholder="Usernames, separated by commas" />
                            <Button disabled={!s.field.trim()} type="submit" variant="ghost">Add</Button>
                        </form>
                    )}
                    {mod && (
                        <div class="chat-invite">
                            {c.invite && c.invite.id === id
                                ? (
                                    <>
                                        <code class="chat-invite-link">{inviteLink(c.invite.code)}</code>
                                        <Button
                                            onclick={() => void navigator.clipboard?.writeText(inviteLink(c.invite!.code))}
                                            size="sm"
                                            variant="ghost"
                                        >
                                            Copy
                                        </Button>
                                        <Button onclick={() => c.createInvite(id)} size="sm" variant="ghost">New link</Button>
                                        <p class="chat-hint">
                                            Whoever has the link joins straight away, for 14 days. People removed with a
                                            block still can't.
                                        </p>
                                    </>
                                )
                                : <Button onclick={() => c.createInvite(id)} size="sm" variant="ghost">Make an invite link</Button>}
                        </div>
                    )}
                    <div class="chat-leave">
                        {community.role === 'owner'
                            ? <p class="chat-hint">You own this community. Hand it over (a person's menu) before leaving.</p>
                            : (
                                <Button
                                    onclick={() => {
                                        if (confirm(`Leave ${settings.name}? You leave every topic in it.`)) {
                                            c.leaveCommunity(id)
                                            this.show({kind: 'welcome'})
                                        }
                                    }}
                                    variant="ghost"
                                >
                                    Leave the community
                                </Button>
                            )}
                    </div>
                </div>
            </>
        )
    }

    editView(id: string) {
        const c = chat()
        const s = this.s
        const d = s.community
        const community = c.communities.find((x) => x.id === id)
        if (!community) return this.head('Edit community', 'You are no longer in it')
        return (
            <>
                {this.head('Edit community', community.settings.name)}
                <form
                    class="chat-pane chat-form"
                    onsubmit={(e: Event) => {
                        e.preventDefault()
                        c.editCommunity(id, settingsOf(d))
                        d.topicEdits.forEach((t, i) => {
                            const before = community.topics.find((x) => x.channel === t.channel)
                            if (before && (before.name !== t.name || before.archived !== t.archived)) {
                                c.editTopic(id, t.channel, t.name, i, t.archived)
                            }
                        })
                        this.show({id, kind: 'community'})
                    }}
                >
                    {this.settingsForm(d)}
                    {d.topicEdits.length > 0 && <h3 class="chat-pane-h">Topics</h3>}
                    {d.topicEdits.map((t) => (
                        <div class="chat-inline" key={t.channel}>
                            <FieldText label="#" model={bind(t, 'name')} />
                            <FieldCheckbox label="Archived" model={bind(t, 'archived')} />
                        </div>
                    ))}
                    <Button type="submit" variant="primary">Save</Button>
                    {community.role === 'owner' && (
                        <Button
                            onclick={(e: Event) => {
                                e.preventDefault()
                                if (confirm(`Delete ${community.settings.name} and every topic in it? This cannot be undone.`)) {
                                    c.deleteCommunity(id)
                                    this.show({kind: 'welcome'})
                                }
                            }}
                            variant="ghost"
                        >
                            Delete the community
                        </Button>
                    )}
                </form>
            </>
        )
    }

    messageList(channel: string, list: Message[], replies: Map<number, Message[]> | null, me: string | null) {
        const out: m.Children[] = []
        let lastDay = ''
        let prev: Message | null = null
        let toldShared = false
        for (const msg of list) {
            // History from before this device joined is somebody's copy, not
            // the log, and says so once, above the first of it.
            if (msg.sharedBy && !toldShared) {
                toldShared = true
                out.push(
                    <div class="chat-day" key="shared">
                        Earlier messages, shared by {shown(msg.sharedBy, me)} when you joined
                    </div>,
                )
            }
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
        const topic = c.topicOf(channel)
        const mod = moderates(topic?.community)
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
                            <b class={mine ? '' : 'chat-who'} onclick={() => !mine && this.person(msg.from, channel)}>
                                {shown(msg.from, me)}
                            </b>
                            <span class="chat-msg-time">{time(msg.sentAtMs)}</span>
                        </div>
                    )}
                    {msg.deleted ? (
                        <p class="chat-msg-text is-deleted">
                            {msg.removedBy
                                ? `Removed by a moderator (${shown(msg.removedBy, me)}).`
                                : 'This message was deleted.'}
                        </p>
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
                        {!mine && topic && (
                            <button onclick={() => this.report(channel, msg)} title="Report to the moderators" type="button">
                                ⚑
                            </button>
                        )}
                        {!mine && mod && (
                            <button
                                onclick={() => {
                                    if (confirm('Delete this message for everyone? You moderate this community.')) {
                                        c.moderateDelete(channel, msg.seq!)
                                    }
                                }}
                                title="Delete for everyone (moderator)"
                                type="button"
                            >
                                🛡
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

    dialogView() {
        const c = chat()
        const s = this.s
        const me = c.address
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
            case 'direct':
                return form(
                    'Message a person',
                    <>
                        <p class="chat-hint">A username on this server, or a full address. A first message to somebody who is not a contact yet arrives as a request.</p>
                        {input('To', 'field', 'sanne')}
                    </>,
                    'Start',
                    () => c.startDirect(s.field.trim()),
                    !!s.field.trim(),
                )
            case 'group':
                return form(
                    'Start a group',
                    <>
                        <p class="chat-hint">A private chat with a few people you pick. Only they know it exists.</p>
                        {input('Name', 'field', 'Weekend trip')}
                        {input('People', 'field2', 'Usernames, separated by commas')}
                    </>,
                    'Start',
                    () => c.startGroup(s.field.trim(), list(s.field2)),
                    !!s.field.trim() && !!s.field2.trim(),
                )
            case 'members': {
                const channel = s.pane.kind === 'chat' ? s.pane.channel : null
                const sum = c.chats.find((x) => x.channel === channel)
                return form(
                    'People in this chat',
                    <>
                        <ul class="chat-people">
                            {me && <li>{Avatar(me, 22)} {shown(me, me)} (you)</li>}
                            {(sum?.people ?? []).map((p) => <li key={p}>{Avatar(p, 22)} {shown(p, me)}</li>)}
                        </ul>
                        {input('Add somebody', 'field', 'username')}
                    </>,
                    'Add',
                    () => channel && c.addToChat(channel, s.field.trim()),
                    !!s.field.trim(),
                )
            }
            case 'report':
                return form(
                    'Report to the moderators',
                    <>
                        <p class="chat-hint">
                            This goes to the community's moderators, with your copy of the line: they cannot read
                            the topic otherwise.
                        </p>
                        <blockquote class="chat-quote">{s.report.line}</blockquote>
                        <FieldSelect label="Why" model={bind(s.report, 'reason')} options={REASONS} />
                        <FieldText label="Anything else (optional)" model={bind(s.report, 'note')} />
                    </>,
                    'Send report',
                    () => {
                        const r = s.report
                        c.reportToModerators(r.channel, r.seq, r.who, r.reason, r.note, r.line)
                    },
                )
            case 'person':
                return this.personDialog(open, close)
        }
        return null
    }

    /**
     * One menu for a person, with a section for each role this account has
     * (`docs/mockups/communities/v3.html`, Q and R): what you cannot do is not
     * there.
     */
    personDialog(open: {value: boolean}, close: () => void) {
        const c = chat()
        const s = this.s
        const me = c.address
        const who = s.person.address
        const name = shown(who, me)
        const fromTopic = s.person.channel ? c.topicOf(s.person.channel) : null
        // The communities this account moderates that they are in.
        const ours = c.communities.filter((x) => moderates(x) && x.members.some((p) => p.address === who))
        const direct = c.chats.find((x) => !x.group && x.label.toLowerCase() === who.toLowerCase())
        const act = (f: () => void) => () => {
            f()
            close()
        }
        return (
            <Dialog class="chat-dialog" footer={<Button onclick={close} variant="ghost">Close</Button>} model={open} title={name}>
                <div class="chat-menu">
                    <Button
                        onclick={act(() => (direct ? this.open(direct.channel) : c.startDirect(who)))}
                        variant="ghost"
                    >
                        Message {name}
                    </Button>
                    {ours.map((community) => {
                        const theirs = community.members.find((p) => p.address === who)!.role
                        const owner = community.role === 'owner'
                        const mayRemove = theirs === 'member' || (theirs === 'moderator' && owner)
                        return (
                            <section key={community.id}>
                                <h4>In {community.settings.name} <span class="chat-role">{ROLE[community.role!]}</span></h4>
                                {owner && theirs === 'member' && (
                                    <Button onclick={act(() => c.setCommunityRole(community.id, who, 'moderator'))} variant="ghost">
                                        Make moderator
                                    </Button>
                                )}
                                {owner && theirs === 'moderator' && (
                                    <Button onclick={act(() => c.setCommunityRole(community.id, who, 'member'))} variant="ghost">
                                        Stop being a moderator
                                    </Button>
                                )}
                                {owner && (
                                    <Button
                                        onclick={act(() => {
                                            if (confirm(`Hand ${community.settings.name} over to ${name}? You become a moderator.`)) {
                                                c.setCommunityRole(community.id, who, 'owner')
                                            }
                                        })}
                                        variant="ghost"
                                    >
                                        Hand the community over
                                    </Button>
                                )}
                                {mayRemove && fromTopic?.community.id === community.id && (
                                    <Button
                                        onclick={act(() => c.removeFromCommunity(community.id, who, fromTopic.topic.channel, false))}
                                        variant="ghost"
                                    >
                                        Remove from # {fromTopic.topic.name}
                                    </Button>
                                )}
                                {mayRemove && (
                                    <Button onclick={act(() => c.removeFromCommunity(community.id, who, '', false))} variant="ghost">
                                        Remove from the community
                                    </Button>
                                )}
                                {mayRemove && (
                                    <Button onclick={act(() => c.removeFromCommunity(community.id, who, '', true))} variant="ghost">
                                        Remove and block from rejoining
                                    </Button>
                                )}
                            </section>
                        )
                    })}
                    {s.admin && (
                        <section>
                            <h4>On this server <span class="chat-role">you are an admin</span></h4>
                            <Button onclick={act(() => void adminAct(who, false))} variant="ghost">Suspend account</Button>
                            <Button
                                onclick={act(() => {
                                    if (confirm(`Delete ${name}'s account? It cannot be undone.`)) void adminAct(who, true)
                                })}
                                variant="ghost"
                            >
                                Delete account
                            </Button>
                            <p class="chat-hint">Reports about them are on the {Link(ADMIN, 'operators’ pages')}.</p>
                        </section>
                    )}
                </div>
            </Dialog>
        )
    }
}

/** A <details> menu stays open after a choice unless it is told. */
function closeMenus() {
    for (const el of document.querySelectorAll('details.chat-new[open]')) el.removeAttribute('open')
}

/** What a directory row says about joining. */
function listingState(l: CommunityListing): string {
    switch (l.mine) {
        case 'member':
            return 'you’re in'
        case 'asked':
            return 'asked'
        case 'refused':
            return 'can’t join'
        default:
            return RULE[l.join]
    }
}

/**
 * Suspend (`delete` false) or delete somebody's account, as the operators'
 * account page does, with this person's own session: the server checks they
 * may.
 */
async function adminAct(address: string, remove: boolean) {
    const c = chat()
    const name = address.split('@')[0]
    try {
        const {accounts} = await signupApi.get<{accounts: {id: string; username: string}[]}>('/api/admin/accounts')
        const account = accounts.find((a) => a.username.toLowerCase() === name.toLowerCase())
        if (!account) {
            c.notice = `No account called ${name} on this server.`
        } else if (remove) {
            await signupApi.delete(`/api/admin/accounts/${account.id}`)
            c.notice = `Deleted ${name}'s account.`
        } else {
            await signupApi.patch(`/api/admin/accounts/${account.id}`, {disabled: true})
            c.notice = `Suspended ${name}: they can't sign in until an admin lifts it.`
        }
    } catch (e) {
        c.notice = `Not done: ${(e as Error).message}`
    }
    m.redraw()
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

/** "350 m away", "4.2 km away". */
function distanceWords(m: number): string {
    if (m < 1000) return `${Math.round(m / 10) * 10} m away`
    return m < 10_000 ? `${(m / 1000).toFixed(1)} km away` : `${Math.round(m / 1000)} km away`
}

/** A place's name, or its coordinates when it was given none. */
function placeWords(p: CommunityPlace): string {
    return p.name.trim() || `${p.latitude.toFixed(4)}, ${p.longitude.toFixed(4)}`
}

/** The code in an invite link (`…/go?community=CODE`), or a bare code. */
function inviteCode(input: string): string | null {
    const text = input.trim()
    let code = text
    if (/^[a-z]+:\/\//i.test(text)) {
        try {
            code = new URL(text).searchParams.get('community') ?? ''
        } catch {
            return null
        }
    }
    return /^[A-Za-z0-9]{1,64}$/.test(code) ? code : null
}

/** The link that opens an invite, in the app on a phone that has it and here otherwise. */
function inviteLink(code: string): string {
    return `https://cartopolis.org/go?community=${code}`
}
