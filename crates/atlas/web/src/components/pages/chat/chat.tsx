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

/**
 * Stand a client in for the real one, before the page is first drawn: how a
 * preview draws a call with several people in it on a machine with nobody to
 * call.
 */
export function standInClient(stand: ChatClient): void {
    client = stand
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

/**
 * The call screen's glyphs: Material Symbols, the set the app draws its call
 * screen in (`systems::icons`), as inline paths so the page needs no icon font.
 */
const ICONS: Record<string, string> = {
    arrow_back: 'M20 11H7.83l5.59-5.59L12 4l-8 8 8 8 1.41-1.41L7.83 13H20v-2z',
    call: 'M20.01 15.38c-1.23 0-2.42-.2-3.53-.56a.977.977 0 0 0-1.01.24l-1.57 1.97c-2.83-1.35-5.48-3.9-6.89-6.83l1.95-1.66c.27-.28.35-.67.24-1.02-.37-1.11-.56-2.3-.56-3.53 0-.54-.45-.99-.99-.99H4.19C3.65 3 3 3.24 3 3.99 3 13.28 10.73 21 20.01 21c.71 0 .99-.63.99-1.18v-3.45c0-.54-.45-.99-.99-.99z',
    call_end: 'M12 9c-1.6 0-3.15.25-4.6.72v3.1c0 .39-.23.74-.56.9-.98.49-1.87 1.12-2.66 1.85-.18.18-.43.28-.7.28-.28 0-.53-.11-.71-.29L.29 13.08a.956.956 0 0 1-.29-.7c0-.28.11-.53.29-.71C3.34 8.78 7.46 7 12 7s8.66 1.78 11.71 4.67c.18.18.29.43.29.71 0 .28-.11.53-.29.71l-2.48 2.48c-.18.18-.43.29-.71.29-.27 0-.52-.11-.7-.28a11.27 11.27 0 0 0-2.67-1.85.996.996 0 0 1-.56-.9v-3.1C15.15 9.25 13.6 9 12 9z',
    chat: 'M20 2H4c-1.1 0-1.99.9-1.99 2L2 22l4-4h14c1.1 0 2-.9 2-2V4c0-1.1-.9-2-2-2zM6 9h12v2H6V9zm8 5H6v-2h8v2zm4-6H6V6h12v2z',
    mic: 'M12 14c1.66 0 2.99-1.34 2.99-3L15 5c0-1.66-1.34-3-3-3S9 3.34 9 5v6c0 1.66 1.34 3 3 3zm5.3-3c0 3-2.54 5.1-5.3 5.1S6.7 14 6.7 11H5c0 3.41 2.72 6.23 6 6.72V21h2v-3.28c3.28-.48 6-3.3 6-6.72h-1.7z',
    mic_off: 'M19 11h-1.7c0 .74-.16 1.43-.43 2.05l1.23 1.23c.56-.98.9-2.09.9-3.28zm-4.02.17c0-.06.02-.11.02-.17V5c0-1.66-1.34-3-3-3S9 3.34 9 5v.18l5.98 5.99zM4.27 3L3 4.27l6.01 6.01V11c0 1.66 1.33 3 2.99 3 .22 0 .44-.03.65-.08l1.66 1.66c-.71.33-1.5.52-2.31.52-2.76 0-5.3-2.1-5.3-5.1H5c0 3.41 2.72 6.23 6 6.72V21h2v-3.28c.91-.13 1.77-.45 2.54-.9L19.73 21 21 19.73 4.27 3z',
    swap: 'M6.99 11L3 15l3.99 4v-3H14v-2H6.99v-3zM21 9l-3.99-4v3H10v2h7.01v3L21 9z',
    videocam: 'M17 10.5V7c0-.55-.45-1-1-1H4c-.55 0-1 .45-1 1v10c0 .55.45 1 1 1h12c.55 0 1-.45 1-1v-3.5l4 4v-11l-4 4z',
    videocam_off: 'M21 6.5l-4 4V7c0-.55-.45-1-1-1H9.82L21 17.18V6.5zM3.27 2L2 3.27 4.73 6H4c-.55 0-1 .45-1 1v10c0 .55.45 1 1 1h12c.21 0 .39-.08.54-.18L19.73 21 21 19.73 3.27 2z',
}

function Icon(name: string) {
    return (
        <svg aria-hidden="true" class="chat-icon" viewBox="0 0 24 24">
            <path d={ICONS[name]} fill="currentColor" />
        </svg>
    )
}

/**
 * A round call key with its label under it — `theme::call_button` in the app,
 * with the same four tones: green to answer, red to hang up, frosted glass for
 * a switch that is on, and the solid cream plate for one that is off (the
 * state worth noticing).
 */
function CallKey(
    icon: string,
    label: string,
    tone: 'accept' | 'hangup' | 'glass' | 'off',
    onclick: () => void,
    size: 'sm' | 'md' | 'lg' = 'md',
) {
    return (
        <button aria-label={label} class={`chat-key is-${tone} is-${size}`} onclick={onclick} title={label}>
            <span class="chat-key-disc">{Icon(icon)}</span>
            {size !== 'sm' && <span class="chat-key-label">{label}</span>}
        </button>
    )
}

/** How long the call has run, as `m:ss`. */
function callTime(calls: ChatClient['calls']): string {
    const a = calls.active
    if (!a || a.since === null) return '0:00'
    const secs = Math.floor((Date.now() - a.since) / 1000)
    return `${Math.floor(secs / 60)}:${String(secs % 60).padStart(2, '0')}`
}

/** What the call is doing, under its name. */
function callStatus(calls: ChatClient['calls']): string {
    const a = calls.active
    if (!a) return ''
    if (!a.connected) return 'Connecting…'
    return a.since === null ? 'Ringing…' : callTime(calls)
}

/**
 * Columns for `n` pictures: the count that keeps each as large as it can be.
 * In the middle (`wide`) that is the squarest grid; in the side panel, which is
 * narrow and tall, one column until a third person and two after.
 */
export function gridColumns(n: number, wide: boolean): number {
    if (!wide) return n <= 2 ? 1 : 2
    return Math.max(1, Math.ceil(Math.sqrt(n)))
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
    /** The call is the wide view in the middle and the chat sits beside it. */
    callFocus: boolean
    /** A ringing call folded down to a banner, by channel. */
    ringFolded: string | null
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
        callFocus: false,
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
        ringFolded: null,
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
        // A call folded to a banner stays folded only while it rings: the next
        // call in that chat comes in as the dialog again.
        if (this.s.ringFolded !== null && chat().calls.ringing() !== this.s.ringFolded) this.s.ringFolded = null
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
        const inCall = chat().calls.active !== null
        return (
            <div
                class={[
                    'chat',
                    pane.kind === 'welcome' ? '' : 'is-open',
                    inCall ? 'is-in-call' : '',
                    inCall && s.callFocus ? 'is-call-focus' : '',
                ].join(' ')}
            >
                {this.sidebar()}
                <main class="chat-main">
                    {inCall && this.callBar()}
                    {pane.kind === 'chat' && this.channelView(pane.channel)}
                    {pane.kind === 'community' && this.communityView(pane.id)}
                    {pane.kind === 'edit' && this.editView(pane.id)}
                    {pane.kind === 'find' && this.findView()}
                    {pane.kind === 'preview' && this.previewView(pane.id)}
                    {pane.kind === 'create' && this.createView()}
                    {pane.kind === 'invite' && this.inviteView(pane.code)}
                    {pane.kind === 'welcome' && this.welcome()}
                </main>
                {this.callStage()}
                {pane.kind === 'chat' && s.thread !== null && this.threadView(pane.channel, s.thread)}
                {this.ringingView()}
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

    /**
     * A private chat's call keys in its header, drawn as the call screen's own
     * round buttons (`systems::call_screen` in the app): green to call, a glass
     * key for video, a green pill to join a call already going on. Inside the
     * call this chat is in, the controls live on the call itself, so the header
     * only says so.
     */
    callButtons(channel: string) {
        const calls = chat().calls
        if (calls.active?.channel === channel) {
            return (
                <button class="chat-call-pill is-live" onclick={() => this.focusCall(!this.s.callFocus)} title="Show the call">
                    {Icon('call')} In call · {callTime(calls)}
                </button>
            )
        }
        const others = calls.others(channel)
        if (others.length) {
            return (
                <button class="chat-call-pill" onclick={() => this.answer(channel, false)} title="Join the call">
                    {Icon('call')} Join call · {others.length}
                </button>
            )
        }
        return (
            <>
                {CallKey('videocam', 'Video call', 'glass', () => this.answer(channel, true), 'sm')}
                {CallKey('call', 'Call', 'accept', () => this.answer(channel, false), 'sm')}
            </>
        )
    }

    /** Place, answer or join the call in `channel`, and bring it into view. */
    answer(channel: string, camera: boolean) {
        this.s.ringFolded = null
        if (this.s.pane.kind !== 'chat' || this.s.pane.channel !== channel) this.open(channel)
        // A phone has room for one of the two: the call takes the screen.
        this.s.callFocus = matchMedia('(max-width: 760px)').matches
        void chat().calls.join(channel, camera)
    }

    focusCall(on: boolean) {
        this.s.callFocus = on
    }

    /**
     * The call this page is in, as a panel beside the chat — or as the wide view
     * in the middle with the chat beside it (`callFocus`), swapped by one button.
     *
     * Its people are a balanced grid, not a masonry: every picture is a camera
     * frame of the same shape, so the layout's job is to pick a column count
     * that keeps them as large as the space allows (`gridColumns`), as Meet and
     * Jitsi do. One other person fills the panel and this device's own picture
     * is a small window in its corner, as on the app's call screen.
     */
    callStage() {
        const c = chat()
        const calls = c.calls
        const a = calls.active
        if (!a) return null
        const s = this.s
        const others = calls.others(a.channel)
        const self = calls.selfView()
        const me = c.address ?? 'You'
        const name = this.chatTitle(a.channel)
        const people = others.map((o) => {
            const t = [...a.tiles.values()].find((x) => x.participant === o.participant)
            return {audio: t?.audio ?? null, key: `p${o.participant}`, name: o.username, video: t?.video ?? null}
        })
        // Pictures and sound from somebody the roster has not named yet.
        for (const t of a.tiles.values()) {
            if (!others.some((o) => o.participant === t.participant)) {
                people.push({audio: t.audio, key: `p${t.participant}`, name: '…', video: t.video})
            }
        }
        const pip = people.length <= 1
        const tiles = pip ? people : [...people, {audio: null, key: 'self', name: 'You', video: self}]
        const cols = gridColumns(Math.max(tiles.length, 1), s.callFocus)
        const tile = (p: {audio: MediaStream | null; key: string; name: string; video: MediaStream | null}, isSelf: boolean) => (
            <figure class={isSelf ? 'chat-call-tile is-self' : 'chat-call-tile'} key={p.key}>
                {p.video
                    ? Media(p.video, 'video', true)
                    : <span class="chat-call-face">{Avatar(isSelf ? me : p.name, 72)}</span>}
                {p.audio && Media(p.audio, 'audio', false)}
                <figcaption>{p.name}</figcaption>
            </figure>
        )
        return (
            <aside class="chat-call">
                <header class="chat-call-head">
                    <button class="chat-call-title" onclick={() => this.open(a.channel)} title="Open this chat">
                        <b>{name}</b>
                        <span class="chat-sub">🔐 {callStatus(calls)}</span>
                    </button>
                    <button
                        class="chat-call-swap"
                        onclick={() => this.focusCall(!s.callFocus)}
                        title={s.callFocus ? 'Put the chat back in the middle' : 'Put the call in the middle'}
                    >
                        {Icon(s.callFocus ? 'chat' : 'swap')}
                        <span>{s.callFocus ? 'Chat' : 'Swap'}</span>
                    </button>
                </header>
                <div class={pip ? 'chat-call-grid is-pair' : 'chat-call-grid'} style={`--cols:${cols}`}>
                    {people.length === 0 && (
                        <div class="chat-call-waiting">
                            {Avatar(name, 96)}
                            <span>{a.since === null ? 'Ringing…' : 'Waiting for their picture and sound…'}</span>
                        </div>
                    )}
                    {tiles.map((p) => tile(p, p.key === 'self'))}
                    {pip && self && <div class="chat-call-pip">{tile({audio: null, key: 'self', name: 'You', video: self}, true)}</div>}
                </div>
                <footer class="chat-call-controls">
                    {CallKey(a.camera ? 'videocam' : 'videocam_off', a.camera ? 'Camera' : 'Camera off', a.camera ? 'glass' : 'off', () => void calls.setCamera(!a.camera))}
                    {CallKey(a.muted ? 'mic_off' : 'mic', a.muted ? 'Unmute' : 'Mute', a.muted ? 'off' : 'glass', () => calls.setMuted(!a.muted))}
                    {CallKey('call_end', 'End', 'hangup', () => calls.hangUp())}
                </footer>
            </aside>
        )
    }

    /** On a phone the call takes the screen; folded, it is this green bar at the top. */
    callBar() {
        const calls = chat().calls
        return (
            <button class="chat-call-bar" onclick={() => this.focusCall(true)}>
                {Icon('call')}
                <span>{this.chatTitle(calls.active!.channel)} · {callTime(calls)}</span>
                <span class="chat-call-bar-hint">Tap to return to the call</span>
            </button>
        )
    }

    /** A chat's name, as its header shows it. */
    chatTitle(channel: string): string {
        const c = chat()
        const topic = c.topicOf(channel)
        if (topic) return `# ${topic.topic.name}`
        const sum = c.chats.find((x) => x.channel === channel)
        return sum ? (sum.group ? sum.label : shown(sum.label, c.address)) : 'Call'
    }

    /**
     * A call ringing in one of this person's chats, wherever they are on the
     * page: the app's incoming-call screen as a dialog — who, large, on a
     * pulsing initial; Decline in red, Answer in green, Video between — and,
     * over a call already going on or once folded away, a banner with the same
     * keys at a smaller size.
     */
    ringingView() {
        const c = chat()
        const s = this.s
        const channel = c.calls.ringing()
        if (!channel) return null
        const caller = c.calls.others(channel)[0]?.username ?? 'Somebody'
        const where = this.chatTitle(channel)
        const what = where.toLowerCase() === caller.toLowerCase() ? 'Incoming call' : `Incoming call · ${where}`
        const decline = () => {
            s.ringFolded = null
            c.calls.decline(channel)
        }
        if (c.calls.active || s.ringFolded === channel) {
            return (
                <div class="chat-ring-banner" role="alertdialog" aria-label={`${caller} is calling`}>
                    <button class="chat-ring-banner-who" onclick={() => (s.ringFolded = null)} title="Show the call">
                        {Avatar(caller, 36)}
                        <span>
                            <b>{caller}</b>
                            <span class="chat-sub">{what}</span>
                        </span>
                    </button>
                    {CallKey('call_end', 'Decline', 'hangup', decline, 'sm')}
                    {CallKey('call', 'Answer', 'accept', () => this.answer(channel, false), 'sm')}
                </div>
            )
        }
        return (
            <div class="chat-ring" role="alertdialog" aria-label={`${caller} is calling`}>
                <section class="chat-ring-card">
                    <button class="chat-ring-fold" onclick={() => (s.ringFolded = channel)} title="Not now — keep it ringing as a banner">
                        {Icon('arrow_back')}
                    </button>
                    <div class="chat-ring-face">{Avatar(caller, 120)}</div>
                    <h2>{caller}</h2>
                    <p>{what}</p>
                    <p class="chat-sub">🔐 End-to-end encrypted</p>
                    <div class="chat-ring-keys">
                        {CallKey('call_end', 'Decline', 'hangup', decline, 'lg')}
                        {CallKey('videocam', 'Video', 'glass', () => this.answer(channel, true), 'lg')}
                        {CallKey('call', 'Answer', 'accept', () => this.answer(channel, false), 'lg')}
                    </div>
                </section>
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

/**
 * Ask once, on a click, for permission to show notifications. Allowed, the
 * browser is also subscribed to be rung for calls while the page is closed.
 */
function askForNotifications() {
    if (typeof Notification !== 'undefined' && Notification.permission === 'default') {
        void Notification.requestPermission().then((answer) => {
            if (answer === 'granted') void chat().push.enable()
        })
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
