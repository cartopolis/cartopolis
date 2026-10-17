import m, {MithrilComponent} from '@garage44/mithril'
import {Button, Card, CloseButton, Dialog, FieldCheckbox, FieldSelect, FieldText, ThemeSwitch} from '@garage44/mithril/components'
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
import type {CallMember} from '@/lib/call'
import {CAPTION_LANGUAGES} from '@/lib/call-captions'
import {bind, local} from '@/lib/reactive'
import {ADMIN} from '@/lib/base'
import {playerSignIn, playerSignOut} from '@/lib/signup'
import {adoptSession, dropOperator} from '@/lib/accounts'
import {layout} from '@/lib/layout'
import {AdminArea, adminCrumbs} from '@/components/elements/admin/admin'
import {Icon} from '@/lib/icons'
import {Posts, Tabs, tabOf} from './posts'

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

/** The switch's three positions, and the path each one lives at. */
type Mode = 'community' | 'world' | 'admin'

const PATHS: Record<Mode, string> = {admin: ADMIN, community: '/', world: '/world'}
const LABELS: Record<Mode, string> = {admin: 'Admin', community: 'Community', world: 'World'}

/** Which position the address is at: `/world`, `/admin`, or Community for the rest. */
function mode(): Mode {
    const path = (m.route.get() ?? '/').split('?')[0]
    if (path === '/world' || path.startsWith('/world/')) return 'world'
    if (path === ADMIN || path.startsWith(`${ADMIN}/`)) return 'admin'
    return 'community'
}

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
    const url = pictureUrl(faceHash(who))
    if (url) {
        return <img alt="" class="chat-avatar" src={url} style={`width:${size}px;height:${size}px;object-fit:cover`} />
    }
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


/** The reactions a call offers, in the order the picker shows them. */
const CALL_REACTIONS = ['👍', '👏', '😂', '❤️', '🎉', '😮']

/** Somebody on the call's stage; `participant` -1 is this device. */
interface CallPerson {
    audio: MediaStream | null
    key: string
    member: CallMember | null
    name: string
    participant: number
    video: MediaStream | null
}

/**
 * One person on the stage: their picture or their initial, what they say
 * (their sound plays from here), and what the call knows about them — talking
 * (a teal edge), muted or held by a host, a raised hand's place in the queue,
 * a host's badge, a reaction floating up.
 */
function callTile(calls: ChatClient['calls'], p: CallPerson) {
    const isSelf = p.participant === -1
    const talking = calls.speaking(p.participant)
    const reaction = calls.reactionOf(p.participant)
    const who = p.member
    const muted = isSelf ? calls.active?.muted : who?.muted
    return (
        <figure class={['chat-call-tile', isSelf ? 'is-self' : '', talking ? 'is-talking' : ''].join(' ')} key={p.key}>
            {p.video ? Media(p.video, 'video', true) : <span class="chat-call-face">{Avatar(isSelf ? (chat().address ?? 'You') : p.name, 72)}</span>}
            {p.audio && Media(p.audio, 'audio', false)}
            {who?.hand != null && (
                <span class="meet-tile-hand" title="Hand raised">
                    {Icon('back_hand')} {calls.members().filter((x) => x.hand !== null && x.hand <= (who.hand ?? 0)).length}
                </span>
            )}
            {/* Keyed by the reaction, in a fragment of its own, so a new one restarts the rise. */}
            {[
                reaction && (
                    <span class="meet-tile-react" key={reaction.id}>
                        {reaction.emoji}
                    </span>
                ),
            ]}
            <figcaption>
                {who?.held ? <span class="meet-mic is-held">{Icon('lock')}</span> : muted ? <span class="meet-mic is-off">{Icon('mic_off')}</span> : null}
                {p.name}
                {who && who.role !== 'member' && <span class="meet-tile-host" title={who.role === 'host' ? 'Host' : 'Co-host'}>{Icon('shield_person')}</span>}
            </figcaption>
        </figure>
    )
}

/** The check before joining's own camera and microphone: opened when it opens,
 *  stopped before the call opens its own. */
let preview: {analyser: AnalyserNode; audio: AudioContext; stream: MediaStream} | null = null
let previewTimer: ReturnType<typeof setInterval> | null = null

async function startPreview(video: string | null, audio: string | null, camera: boolean): Promise<void> {
    stopPreview()
    try {
        const stream = await navigator.mediaDevices.getUserMedia({
            audio: audio ? {deviceId: {exact: audio}} : true,
            video: camera ? {height: 360, width: 640, ...(video ? {deviceId: {exact: video}} : {})} : false,
        })
        const ctx = new AudioContext()
        const analyser = ctx.createAnalyser()
        analyser.fftSize = 512
        ctx.createMediaStreamSource(stream).connect(analyser)
        preview = {analyser, audio: ctx, stream}
        // The meter moves without anybody touching the page.
        previewTimer = setInterval(() => m.redraw(), 120)
    } catch {
        preview = null
    }
    m.redraw()
}

function stopPreview(): void {
    if (previewTimer) clearInterval(previewTimer)
    previewTimer = null
    preview?.stream.getTracks().forEach((t) => t.stop())
    void preview?.audio.close()
    preview = null
}

/** The previewed microphone's level now (RMS, full scale 1). */
function previewLevel(): number {
    if (!preview) return 0
    const buf = new Float32Array(512)
    preview.analyser.getFloatTimeDomainData(buf)
    let sum = 0
    for (const v of buf) sum += v * v
    return Math.sqrt(sum / buf.length)
}

/** Whether the previewed camera can blur its background itself. */
function previewBlurs(): boolean {
    const t = preview?.stream.getVideoTracks()[0]
    const caps = t?.getCapabilities?.() as {backgroundBlur?: boolean[]} | undefined
    return !!caps?.backgroundBlur?.includes(true)
}

/** A short two-note chime, to hear that the speaker works. */
function playTestTone(): void {
    const ctx = new AudioContext()
    for (const [i, f] of [660, 880].entries()) {
        const o = ctx.createOscillator()
        const g = ctx.createGain()
        o.frequency.value = f
        g.gain.setValueAtTime(0.0001, ctx.currentTime + i * 0.18)
        g.gain.exponentialRampToValueAtTime(0.2, ctx.currentTime + i * 0.18 + 0.02)
        g.gain.exponentialRampToValueAtTime(0.0001, ctx.currentTime + i * 0.18 + 0.3)
        o.connect(g).connect(ctx.destination)
        o.start(ctx.currentTime + i * 0.18)
        o.stop(ctx.currentTime + i * 0.18 + 0.32)
    }
    setTimeout(() => void ctx.close(), 1000)
}

/** A captions language's name, from its code. */
function captionLabel(code: string): string {
    return CAPTION_LANGUAGES.find((l) => l.code === code)?.label ?? code
}

/** How long somebody has waited, as a person says it. */
function waitedFor(since: number): string {
    const s = Math.max(0, Math.round((Date.now() - since) / 1000))
    return s < 60 ? `${s} s` : `${Math.floor(s / 60)} min`
}

/** The call's keys on a desktop: M, V, H and C, and Space held to talk while muted. */
let callKeysFor: object | null = null

function installCallKeys(calls: ChatClient['calls'], s: {callMenu: string | null}): void {
    if (callKeysFor === calls) return
    callKeysFor = calls
    let spaceTalk = false
    const typing = (e: KeyboardEvent) => {
        const t = e.target as HTMLElement | null
        return !!t && (t.isContentEditable || ['INPUT', 'TEXTAREA', 'SELECT'].includes(t.tagName))
    }
    document.addEventListener('keydown', (e: KeyboardEvent) => {
        const a = calls.active
        if (!a || typing(e) || e.ctrlKey || e.metaKey || e.altKey) return
        if (e.key === 'Escape' && s.callMenu) {
            s.callMenu = null
            m.redraw()
        } else if (e.code === 'Space' && a.muted && !e.repeat && !calls.self()?.held) {
            spaceTalk = true
            calls.setMuted(false)
            e.preventDefault()
        } else if (e.key === 'm' || e.key === 'M') calls.setMuted(!a.muted)
        else if (e.key === 'v' || e.key === 'V') void calls.setCamera(!a.camera)
        else if (e.key === 'h' || e.key === 'H') calls.raiseHand(calls.self()?.hand == null)
        else if (e.key === 'c' || e.key === 'C') void calls.setCaptions(!calls.captioning())
    })
    document.addEventListener('keyup', (e: KeyboardEvent) => {
        if (e.code === 'Space' && spaceTalk) {
            spaceTalk = false
            calls.setMuted(true)
        }
    })
}

/** Pictures — a community's, a person's — as object URLs, by hash: fetched once, with the session. */
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

/**
 * People's pictures by name, as the app keeps them (`systems::faces`): a hash
 * in the picture store, asked for in batches and asked again every five
 * minutes, which is how somebody's new picture arrives. Only for this server's
 * people: a name on another server means nothing to this one's accounts.
 */
const faces = new Map<string, {at: number; hash: string | null}>()
const faceQueue = new Set<string>()
let faceTimer: ReturnType<typeof setTimeout> | null = null
const FACE_REFRESH_MS = 300_000

function faceHash(who: string): string | null {
    const [name, server] = who.toLowerCase().split('@')
    if (!name || (server && server !== location.hostname.toLowerCase())) return null
    const known = faces.get(name)
    if ((!known || Date.now() - known.at > FACE_REFRESH_MS) && !faceQueue.has(name)) {
        faceQueue.add(name)
        askFaces()
    }
    return known?.hash ?? null
}

function askFaces() {
    if (faceTimer !== null || !$s.signup.token) return
    faceTimer = setTimeout(() => {
        faceTimer = null
        const names = [...faceQueue].slice(0, 100)
        const at = Date.now()
        for (const n of names) {
            faceQueue.delete(n)
            faces.set(n, {at, hash: faces.get(n)?.hash ?? null})
        }
        void fetch(`/api/pictures/people?names=${encodeURIComponent(names.join(','))}`, {
            headers: {Authorization: `Bearer ${$s.signup.token}`},
        })
            .then(async (r) => {
                if (!r.ok) throw new Error(String(r.status))
                const body = (await r.json()) as {pictures: Record<string, string | null>}
                for (const [n, hash] of Object.entries(body.pictures)) faces.set(n, {at, hash})
            })
            .catch(() => {})
            .finally(() => {
                if (faceQueue.size) askFaces()
                m.redraw()
            })
    }, 200)
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
    /**
     * Which tile on the rail is chosen: `'home'` (people and groups) or a
     * community's id. `null` is the phone's first screen, the list of them;
     * a wide screen draws it as Home.
     */
    space: string | null
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
    /** Speaker (whoever talks is large) or Gallery (everyone equal). */
    callLayout: 'speaker' | 'gallery'
    /** The panel beside the meeting when it is in the middle. */
    callPanel: 'people' | null
    /** The call menu or picker open: `react`, `more`, `leave`, or `person<n>`. */
    callMenu: string | null
    /** This browser's microphones and cameras, and the ones chosen. */
    devices: MediaDeviceInfo[]
    audioDevice: string | null
    videoDevice: string | null
    /** Breakout rooms being set up: how many, for how long, free to move. */
    roomCount: number
    roomMinutes: number
    roomMove: boolean
    /** The check before joining, while it is open. */
    preJoin: {camera: boolean; channel: string} | null
    /** What the check chose to join with. */
    preJoinBlur: boolean
    preJoinMuted: boolean
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
        callLayout: 'gallery',
        callMenu: null,
        audioDevice: null,
        devices: [],
        preJoin: null,
        preJoinBlur: false,
        preJoinMuted: false,
        roomCount: 2,
        roomMinutes: 10,
        roomMove: true,
        videoDevice: null,
        callPanel: null,
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
        space: null,
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

    /**
     * Whether this account may run anything here: Admin then joins the switch,
     * and with `accounts_manage` the person menu has a server section. One
     * session for both (`accounts.adoptSession`).
     */
    async checkAdmin() {
        this.s.adminChecked = true
        await adoptSession()
        this.s.admin = $s.me.admin || $s.me.permissions.includes('accounts_manage')
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
        this.s.space = null
        dropOperator()
        await playerSignOut()
        if (mode() === 'admin') m.route.set('/')
    }

    /** Open a chat; `null` shows the welcome. Its community becomes the rail's choice. */
    open(channel: string | null) {
        if (channel) this.s.space = chat().topicOf(channel)?.community.id ?? 'home'
        this.s.pane = channel ? {kind: 'chat', channel} : {kind: 'welcome'}
        this.s.thread = null
        this.s.editing = null
        chat().open(channel)
        askForNotifications()
    }

    show(pane: Pane) {
        closeMenus()
        if (pane.kind === 'community' || pane.kind === 'edit') this.s.space = pane.id
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

    /**
     * The page (`docs/mockups/web-navigation/`): a top bar holding the switch —
     * Community | World, and Admin for an account that may run something — then
     * the mode below it. Community is the rail, the sidebar, the chat and its
     * thread; World is the app's own web build in a frame, with the topic beside
     * it; Admin is the operator's pages.
     *
     * The three are children at fixed positions rather than one swapped for
     * another, so the World frame never moves in the DOM: moving an iframe
     * reloads it, and this one is a hundred megabytes of WebAssembly.
     */
    view() {
        const signedIn = !!$s.signup.token
        const now = mode()
        if (now === 'world') this.worldOpened = true
        return (
            <div class={`home is-${now}`}>
                {this.topBar(now, signedIn)}
                <div class="home-body">
                    {now === 'community' && (signedIn ? this.communityView() : this.signInView())}
                    {now === 'admin' && (signedIn ? <AdminArea /> : this.signInView())}
                    {now === 'world' && signedIn ? this.worldRail() : null}
                    {this.worldOpened ? this.worldFrame(now === 'world', signedIn) : null}
                </div>
                {signedIn && (
                    <div class="chat chat-overlays">
                        {now !== 'community' && chat().calls.active !== null && this.callBar()}
                        {this.ringingView()}
                        {this.s.preJoin && this.preJoinView()}
                        {this.s.dialog && this.dialogView()}
                    </div>
                )}
            </div>
        )
    }

    /** Community: the rail, the open community's sidebar, the chat, its thread. */
    communityView() {
        const s = this.s
        const pane = s.pane
        const inCall = chat().calls.active !== null
        return (
            <div
                class={[
                    'chat',
                    pane.kind === 'welcome' ? '' : 'is-open',
                    s.space === null ? 'is-spaces' : '',
                    inCall ? 'is-in-call' : '',
                    inCall && s.callFocus ? 'is-call-focus' : '',
                    inCall && s.callFocus && s.callPanel === 'people' ? 'is-call-people' : '',
                ].join(' ')}
            >
                {this.rail()}
                {this.sidebar()}
                <main class="chat-main">
                    {inCall && this.callBar()}
                    {pane.kind === 'chat' && this.channelView(pane.channel)}
                    {pane.kind === 'community' && this.communityPane(pane.id)}
                    {pane.kind === 'edit' && this.editView(pane.id)}
                    {pane.kind === 'find' && this.findView()}
                    {pane.kind === 'preview' && this.previewView(pane.id)}
                    {pane.kind === 'create' && this.createView()}
                    {pane.kind === 'invite' && this.inviteView(pane.code)}
                    {pane.kind === 'welcome' && this.welcome()}
                </main>
                {this.callStage()}
                {pane.kind === 'chat' && s.thread !== null && this.threadView(pane.channel, s.thread)}
            </div>
        )
    }

    /** Whether World has been opened on this page: from then on its frame stays loaded. */
    worldOpened = false
    /** The place the World frame was last sent to, so a redraw does not send it again. */
    worldPlace: string | null = null

    /** A public post's place that asked to be opened in World, as a link's `place=` value. */
    postPlace: string | null = null

    /**
     * The place a rail choice stands for in World: its community's pin, if it
     * has one — or the public post whose place was just opened.
     */
    spacePlace(): string | null {
        if (this.postPlace) return this.postPlace
        const s = this.s
        const community = s.space && s.space !== 'home' ? chat().communities.find((x) => x.id === s.space) : null
        const place = community?.settings.place
        return place ? `${place.latitude.toFixed(5)},${place.longitude.toFixed(5)},1200m` : null
    }

    /**
     * World: the app itself (`/viewer/`), in a frame that is made the first
     * time World is chosen and kept from then on, hidden while Community or
     * Admin is showing, so switching back is instant. Choosing a community with
     * a place flies the camera there: the frame starts at it (`?place=`), and a
     * later choice is posted to the running app (`place_link::follow_framed_places`).
     *
     * Signed in, the topic open in Community stays beside it; signed out, it is
     * the map alone.
     */
    worldFrame(shown: boolean, signedIn: boolean) {
        const place = signedIn ? this.spacePlace() : null
        const pane = this.s.pane
        return (
            <div class={shown ? 'home-world' : 'home-world is-hidden'}>
                <iframe
                    allow="fullscreen; geolocation; microphone; camera; clipboard-write"
                    class="home-world-frame"
                    oncreate={(v: m.VnodeDOM) => {
                        this.worldPlace = place
                        ;(v.dom as HTMLIFrameElement).src = place ? `/viewer/?place=${place}` : '/viewer/'
                    }}
                    onupdate={(v: m.VnodeDOM) => {
                        if (!shown || !place || place === this.worldPlace) return
                        this.worldPlace = place
                        ;(v.dom as HTMLIFrameElement).contentWindow?.postMessage(
                            {place, type: 'cartopolis.place'},
                            location.origin,
                        )
                    }}
                    title="The world"
                />
                {shown && signedIn && (
                    <aside class="chat home-world-chat">
                        {pane.kind === 'chat'
                            ? this.channelView(pane.channel)
                            : <div class="chat-side home-world-list">{this.sideBody()}</div>}
                    </aside>
                )}
            </div>
        )
    }

    /** World's rail: the same tiles as Community's, choosing where the camera goes. */
    worldRail() {
        return <div class="chat home-world-rail">{this.rail()}</div>
    }

    /** The top bar: the wordmark, the switch, where you are, and your account. */
    topBar(now: Mode, signedIn: boolean) {
        const c = signedIn ? chat() : null
        const unread = c
            ? c.chats.filter((x) => !c.isMuted(x.channel)).reduce((n, x) => n + x.unread, 0)
            : 0
        const seg = (to: Mode, label: m.Children, extra = '') => (
            <button
                aria-pressed={now === to ? 'true' : 'false'}
                class={`home-seg-item${now === to ? ' is-on' : ''}${extra}`}
                onclick={() => {
                    if (now !== to) m.route.set(PATHS[to])
                }}
                type="button"
            >
                {label}
            </button>
        )
        return (
            <header class="home-top">
                <a
                    class="home-wordmark"
                    href="/"
                    onclick={(e: Event) => {
                        e.preventDefault()
                        m.route.set('/')
                    }}
                >
                    <img alt="" class="mark-dark" src="/icon.svg" />
                    <img alt="" class="mark-light" src="/icon-light.svg" />
                    <span class="wordmark">carto<b>polis</b></span>
                </a>
                <nav aria-label="Switch between Community, World and Admin" class="home-seg">
                    {seg('community', <>Community{unread > 0 && <span class="home-seg-count">{unread}</span>}</>)}
                    {seg('world', <><span class="home-live" />World</>)}
                    {signedIn && $s.signedIn && seg('admin', 'Admin', ' is-admin')}
                </nav>
                <nav aria-label="Where you are" class="home-crumbs">{this.crumbs(now, signedIn)}</nav>
                <span class="home-spacer" />
                {this.accountMenu(signedIn)}
            </header>
        )
    }

    /** Community › Oosterpoort › # street-party › Thread, each a step back up. */
    crumbs(now: Mode, signedIn: boolean): m.Children {
        const parts: {label: string; go?: () => void}[] = [{label: LABELS[now], go: () => m.route.set(PATHS[now])}]
        if (now === 'admin') {
            for (const label of adminCrumbs()) parts.push({label})
        } else if (signedIn) {
            const c = chat()
            const s = this.s
            // A wide screen draws "nothing chosen yet" as Home; a phone's list has no crumb.
            const space = s.space ?? (matchMedia('(max-width: 760px)').matches ? null : 'home')
            const community = space && space !== 'home' ? c.communities.find((x) => x.id === space) : null
            if (community) {
                parts.push({go: () => this.show({id: community.id, kind: 'community'}), label: community.settings.name})
            } else if (space === 'home') {
                parts.push({go: () => this.open(null), label: 'Home'})
            }
            const pane = s.pane
            if (pane.kind === 'chat') {
                parts.push({go: () => (s.thread = null), label: this.chatTitle(pane.channel)})
                if (s.thread !== null && now === 'community') parts.push({label: 'Thread'})
            } else if (pane.kind === 'find') {
                parts.push({label: 'Find a community'})
            } else if (pane.kind === 'create') {
                parts.push({label: 'New community'})
            } else if (pane.kind === 'edit') {
                parts.push({label: 'Settings'})
            } else if (pane.kind === 'invite' || pane.kind === 'preview') {
                parts.push({label: 'Invitation'})
            }
        }
        return parts.map((p, i) => {
            const last = i === parts.length - 1
            return (
                <>
                    {i > 0 && <span class="home-crumb-sep">›</span>}
                    {last || !p.go
                        ? <span class={last ? 'home-crumb is-last' : 'home-crumb'}>{p.label}</span>
                        : <button class="home-crumb" onclick={p.go} type="button">{p.label}</button>}
                </>
            )
        })
    }

    /** Your account: who you are, the theme, the other ways in, and Sign out. */
    accountMenu(signedIn: boolean) {
        if (!signedIn) {
            return <ThemeSwitch model={layout.theme} />
        }
        const me = $s.signup.username
        return (
            <details class="home-account">
                <summary title={me}>{Avatar(me, 30)}</summary>
                <div class="home-account-menu">
                    <p class="home-account-who">
                        <b>{me}</b>
                        <span>{chat().address ?? 'Signed in'}</span>
                    </p>
                    <ThemeSwitch model={layout.theme} />
                    <nav class="chat-options">
                        {Link('/buildings', <><b>Your buildings</b><span>Claim a building, draw its floors</span></>)}
                        <a href="/apk/">
                            <b>Android app</b>
                            <span>Chat, the world and calls on your phone</span>
                        </a>
                    </nav>
                    <Button onclick={() => void this.signOut()} size="sm" variant="ghost">Sign out</Button>
                </div>
            </details>
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
                            {Link('/world', <><b>Open the map</b><span>The world in your browser, no account needed</span></>)}
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

    /**
     * The rail: Home (people and groups), one tile per community, then Find and
     * New. A wide screen draws it as a column of tiles beside the sidebar; a
     * phone draws it as the first screen, a list with names, and the sidebar is
     * the screen after it.
     */
    rail() {
        const c = chat()
        const s = this.s
        const here = s.space ?? (matchMedia('(max-width: 760px)').matches ? null : 'home')
        const homeUnread = c.chats
            .filter((x) => !c.topicOf(x.channel))
            .reduce((n, x) => n + x.unread, 0)
        const pick = (space: string) => {
            s.space = space
            // A rail choice is where World goes now, not the last post opened.
            this.postPlace = null
            // A wide screen shows a community's page on choosing it; Home shows
            // the welcome until a chat is opened from its list.
            if (space === 'home') this.open(null)
            else this.show({id: space, kind: 'community'})
            // On a phone, choosing goes a level down, to the sidebar.
            s.pane = matchMedia('(max-width: 760px)').matches ? {kind: 'welcome'} : s.pane
        }
        return (
            <nav aria-label="Communities" class="home-rail">
                <h2 class="home-rail-title">Community</h2>
                <button
                    class={here === 'home' ? 'home-tile is-current' : 'home-tile'}
                    onclick={() => pick('home')}
                    title="People and groups"
                    type="button"
                >
                    <span class="home-tile-face is-home">{Icon('chat')}</span>
                    <span class="home-tile-name">Home<span class="chat-sub">People and groups</span></span>
                    {homeUnread > 0 && <span class="chat-count home-tile-badge">{homeUnread}</span>}
                </button>
                {c.communities.map((community) => {
                    const unread = community.topics.filter((t) => !c.isMuted(t.channel)).reduce(
                        (n, t) => n + (c.chats.find((x) => x.channel === t.channel)?.unread ?? 0), 0)
                    const asks = moderates(community) ? community.requests.length + community.reports.length : 0
                    return (
                        <button
                            class={here === community.id ? 'home-tile is-current' : 'home-tile'}
                            key={community.id}
                            onclick={() => pick(community.id)}
                            title={community.settings.name}
                            type="button"
                        >
                            <span class="home-tile-face">{Tile(community.settings.name, 42, community.settings.picture)}</span>
                            <span class="home-tile-name">
                                {community.settings.name}
                                <span class="chat-sub">
                                    {community.role === null ? 'Waiting to be let in' : `${community.members.length} people`}
                                </span>
                            </span>
                            {asks > 0 && <span class="chat-count is-mention home-tile-badge">{asks}</span>}
                            {asks === 0 && unread > 0 && <span class="chat-count home-tile-badge">{unread}</span>}
                        </button>
                    )
                })}
                <button class="home-tile is-tool" onclick={() => this.find()} title="Find a community" type="button">
                    <span class="home-tile-face is-tool">⌕</span>
                    <span class="home-tile-name">Find a community</span>
                </button>
                <details class="chat-new home-tile-new">
                    <summary class="home-tile is-tool" title="New">
                        <span class="home-tile-face is-tool">+</span>
                        <span class="home-tile-name">New</span>
                    </summary>
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
            </nav>
        )
    }

    /** The sidebar: what is inside the rail's choice. */
    sidebar() {
        return (
            <aside class="chat-side">
                <button class="chat-side-back" onclick={() => (this.s.space = null)} type="button">‹ Community</button>
                {this.sideBody()}
                {this.sideFooter()}
            </aside>
        )
    }

    /**
     * Inside a community: its name and place, its topics, and for its owner and
     * moderators the requests and reports waiting. Inside Home: one-to-one chats
     * and groups. Drawn in the sidebar, and in World beside the map when no
     * topic is open there.
     */
    sideBody() {
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
        const community = s.space && s.space !== 'home' ? c.communities.find((x) => x.id === s.space) : null
        if (community) {
            const place = community.settings.place
            const mod = moderates(community)
            const shownPane = s.pane.kind === 'community' && s.pane.id === community.id
            return (
                <>
                    <header class="chat-side-head home-space-head">
                        <button
                            class={shownPane ? 'home-space-name is-current' : 'home-space-name'}
                            onclick={() => this.show({id: community.id, kind: 'community'})}
                            title="About this community"
                            type="button"
                        >
                            {Tile(community.settings.name, 28, community.settings.picture)}
                            <h2>{community.settings.name}</h2>
                            <span class="chat-sub">›</span>
                        </button>
                        <p class="chat-sub home-space-meta">
                            {place ? `📍 ${placeWords(place)} · ` : ''}{community.members.length} people
                        </p>
                        {place && (
                            <Button onclick={() => m.route.set('/world')} size="sm" variant="ghost">
                                See it in World
                            </Button>
                        )}
                    </header>
                    {status && <div class="chat-status">{status}</div>}
                    <section class="chat-section">
                        <h3>Topics</h3>
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
                            )
                        })}
                        {community.role === null && <p class="chat-empty">Waiting for a moderator to let you in.</p>}
                    </section>
                    {mod && (
                        <section class="chat-section home-moderation">
                            <h3>Moderation <span class="chat-role">{ROLE[community.role!]}</span></h3>
                            <button class="chat-item" onclick={() => this.show({id: community.id, kind: 'community'})} type="button">
                                <span class="chat-label">Asking to join</span>
                                {community.requests.length > 0 && <span class="chat-count is-mention">{community.requests.length}</span>}
                            </button>
                            <button class="chat-item" onclick={() => this.show({id: community.id, kind: 'community'})} type="button">
                                <span class="chat-label">Reports</span>
                                {community.reports.length > 0 && <span class="chat-count is-mention">{community.reports.length}</span>}
                            </button>
                        </section>
                    )}
                </>
            )
        }
        const outside = c.chats
            .filter((x) => !c.topicOf(x.channel))
            .sort((a, b) => b.lastAtMs - a.lastAtMs)
        return (
            <>
                <header class="chat-side-head home-space-head">
                    <h2>Home</h2>
                    <p class="chat-sub home-space-meta">People and groups</p>
                </header>
                {status && <div class="chat-status">{status}</div>}
                <section class="chat-section">
                    {outside.length === 0 && <p class="chat-empty">No chats yet. Start one with the + on the left.</p>}
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
            </>
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

    /** What the chat has to say: a refusal, a call that could not start. */
    sideFooter() {
        const c = chat()
        if (!c.notice && !c.calls.notice) return null
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
            </footer>
        )
    }

    welcome() {
        return (
            <div class="chat-welcome">
                <h1>Community</h1>
                <p class="chat-hint">
                    {chat().communities.length
                        ? 'Pick a community on the left, then a topic, or start something with the +.'
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
                <button class="chat-call-pill" onclick={() => this.startCall(channel, false)} title="Join the call">
                    {Icon('call')} Join call · {others.length}
                </button>
            )
        }
        return (
            <>
                {CallKey('videocam', 'Video call', 'glass', () => this.startCall(channel, true), 'sm')}
                {CallKey('call', 'Call', 'accept', () => this.startCall(channel, false), 'sm')}
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

    /**
     * Start or join the call in `channel` from its header. A one-to-one call
     * goes straight in, as a phone does; a group's or a topic's opens the check
     * before joining first (`docs/mockups/meetings/` v2, section 3).
     */
    startCall(channel: string, camera: boolean) {
        const sum = chat().chats.find((c) => c.channel === channel)
        const direct = sum ? !sum.group && !chat().topicOf(channel) : false
        if (direct) {
            this.answer(channel, camera)
            return
        }
        // Joining a meeting already this big, the microphone starts off.
        this.s.preJoinMuted = chat().calls.others(channel).length >= 5
        this.s.preJoinBlur = false
        this.s.preJoin = {camera, channel}
        void startPreview(this.s.videoDevice, this.s.audioDevice, camera).then(() => this.loadDevices())
    }

    /** The check before joining: your picture as the others will see it, the
     *  microphone's level, which devices, blur, and whether to come in muted. */
    preJoinView() {
        const s = this.s
        const p = s.preJoin!
        const c = chat()
        const others = c.calls.others(p.channel)
        const close = () => {
            stopPreview()
            s.preJoin = null
        }
        const join = () => {
            stopPreview()
            s.preJoin = null
            s.ringFolded = null
            if (s.pane.kind !== 'chat' || s.pane.channel !== p.channel) this.open(p.channel)
            s.callFocus = true
            void c.calls.join(p.channel, p.camera, {
                audio: s.audioDevice,
                blur: s.preJoinBlur,
                muted: s.preJoinMuted,
                video: s.videoDevice,
            })
        }
        const level = previewLevel()
        const restart = () => void startPreview(s.videoDevice, s.audioDevice, p.camera)
        return (
            <div class="meet-prejoin" role="dialog" aria-label="Check before joining">
                <section class="meet-prejoin-card">
                    <div class="meet-prejoin-preview">
                        {preview?.stream.getVideoTracks().length
                            ? <div class="chat-call-tile is-self">{Media(preview.stream, 'video', true)}</div>
                            : <div class="chat-call-tile is-off"><span class="chat-call-face">{Avatar(c.address ?? 'You', 96)}</span></div>}
                        <div class="meet-prejoin-keys">
                            <button
                                class={s.preJoinMuted ? 'meet-key is-off' : 'meet-key'}
                                onclick={() => (s.preJoinMuted = !s.preJoinMuted)}
                                title={s.preJoinMuted ? 'Join with the microphone on' : 'Join muted'}
                            >
                                {Icon(s.preJoinMuted ? 'mic_off' : 'mic')}
                            </button>
                            <button
                                class={p.camera ? 'meet-key' : 'meet-key is-off'}
                                onclick={() => {
                                    p.camera = !p.camera
                                    restart()
                                }}
                                title={p.camera ? 'Join without the camera' : 'Join with the camera'}
                            >
                                {Icon(p.camera ? 'videocam' : 'videocam_off')}
                            </button>
                            {previewBlurs() && (
                                <button
                                    class={s.preJoinBlur ? 'meet-key is-sel' : 'meet-key'}
                                    onclick={() => {
                                        s.preJoinBlur = !s.preJoinBlur
                                        void preview?.stream.getVideoTracks()[0]?.applyConstraints({backgroundBlur: s.preJoinBlur} as MediaTrackConstraints)
                                    }}
                                    title="Blur my background"
                                >
                                    {Icon('featured_video')}
                                </button>
                            )}
                        </div>
                    </div>
                    <div class="meet-prejoin-side">
                        <div class="meet-prejoin-what">{this.chatTitle(p.channel)}</div>
                        <h2>{others.length ? 'Join the call' : 'Start a call'}</h2>
                        <p class="chat-sub">
                            {others.length ? `${others.map((o) => o.username).join(', ')} ${others.length === 1 ? 'is' : 'are'} in` : 'Nobody is in yet: they will hear it ring.'}
                        </p>
                        <div class="meet-check">
                            <span class={level > 0.01 ? 'meet-check-ok' : 'meet-check-wait'}>{Icon(level > 0.01 ? 'check' : 'mic')}</span>
                            <label>
                                Microphone
                                <select
                                    onchange={(e: Event) => {
                                        s.audioDevice = (e.target as HTMLSelectElement).value || null
                                        restart()
                                    }}
                                >
                                    {s.devices.filter((d) => d.kind === 'audioinput').map((d) => (
                                        <option selected={d.deviceId === s.audioDevice} value={d.deviceId}>
                                            {d.label || 'Microphone'}
                                        </option>
                                    ))}
                                </select>
                            </label>
                            <span class="meet-level">
                                {[0.01, 0.03, 0.06, 0.1, 0.16].map((t) => (
                                    <i class={level > t ? 'is-on' : ''} />
                                ))}
                            </span>
                        </div>
                        <div class="meet-check">
                            <span class="meet-check-ok">{Icon('check')}</span>
                            <label>
                                Speaker
                                <button class="meet-btn" onclick={() => playTestTone()}>
                                    Play a test sound
                                </button>
                            </label>
                        </div>
                        <div class="meet-check">
                            <span class={p.camera ? 'meet-check-ok' : 'meet-check-wait'}>{Icon(p.camera ? 'check' : 'videocam_off')}</span>
                            <label>
                                Camera
                                <select
                                    onchange={(e: Event) => {
                                        s.videoDevice = (e.target as HTMLSelectElement).value || null
                                        restart()
                                    }}
                                >
                                    {s.devices.filter((d) => d.kind === 'videoinput').map((d) => (
                                        <option selected={d.deviceId === s.videoDevice} value={d.deviceId}>
                                            {d.label || 'Camera'}
                                        </option>
                                    ))}
                                </select>
                            </label>
                        </div>
                        <div class="meet-prejoin-go">
                            <button class="meet-btn" onclick={close}>
                                Cancel
                            </button>
                            <button class="meet-btn is-primary is-big" onclick={join}>
                                {others.length ? 'Join now' : 'Start'}
                            </button>
                        </div>
                        <p class="chat-sub">
                            {s.preJoinMuted ? 'You join muted.' : 'Others hear you as soon as you are in.'} {Icon('lock')} End-to-end encrypted.
                        </p>
                    </div>
                </section>
            </div>
        )
    }

    focusCall(on: boolean) {
        this.s.callFocus = on
    }

    /**
     * The call this page is in (`docs/mockups/meetings/` v2): a panel beside the
     * chat, or, swapped (`callFocus`), the meeting in the middle with the chat or
     * the people beside it.
     *
     * The stage gets the space. A shared screen takes it whole, with everybody in
     * a strip beside it; otherwise Speaker puts whoever is talking large, and
     * Gallery is a balanced grid (`gridColumns`) — every picture a camera frame of
     * one shape, so the layout's only job is a column count, as Meet and Jitsi
     * do. Controls are one dock: microphone, camera, share, react, hand, more,
     * and Leave in red with a word on it. Interruptions are notices with their
     * button in them; a host admits nothing from a panel it has to open.
     */
    callStage() {
        const c = chat()
        const calls = c.calls
        const a = calls.active
        if (!a) return null
        const s = this.s
        installCallKeys(calls, s)
        // The forwarder sends no camera to a page that does not draw one.
        calls.watch(null)
        if (calls.waitingToJoin()) return this.waitingRoom(a.channel)
        // A breakout room shows only who is in it.
        const others = calls.roomOthers()
        const me = calls.self()
        const hosting = calls.hosting()
        const settings = calls.settings()
        const name = this.chatTitle(a.channel)
        const people: CallPerson[] = others.map((o) => {
            const t = a.tiles.get(o.participant)
            return {audio: t?.audio ?? null, key: `p${o.participant}`, member: o, name: o.username, participant: o.participant, video: t?.video ?? null}
        })
        // Pictures and sound from somebody the roster has not named yet.
        for (const t of a.tiles.values()) {
            if (!calls.members().some((o) => o.participant === t.participant)) {
                people.push({audio: t.audio, key: `p${t.participant}`, member: null, name: '…', participant: t.participant, video: t.video})
            }
        }
        const self: CallPerson = {audio: null, key: 'self', member: me, name: 'You', participant: -1, video: calls.selfView()}
        const share = calls.sharer()
        const spotlit = people.find((p) => p.participant === calls.spotlight()) ?? null
        const menu = s.callMenu
        const setMenu = (m: string | null) => (s.callMenu = s.callMenu === m ? null : m)

        // ── The stage ─────────────────────────────────────────────────────
        let stage: m.Children
        if (share) {
            stage = (
                <div class="meet-stage is-share">
                    <div class="meet-screen">
                        {Media(share.stream, 'video', true)}
                        <span class="meet-presenter">
                            {Icon('screen_share')} {share.member.username} is presenting
                        </span>
                    </div>
                    <div class="meet-strip">{[...people, self].map((p) => callTile(calls, p))}</div>
                </div>
            )
        } else if (spotlit || (s.callLayout === 'speaker' && people.length >= 2)) {
            // The hosts' spotlight holds the big tile for everybody.
            const big = spotlit ?? people.find((p) => p.participant === calls.speaker()) ?? people[0]
            stage = (
                <div class="meet-stage is-speaker">
                    <div class="meet-big">{callTile(calls, big)}</div>
                    <div class="meet-strip is-row">{[...people.filter((p) => p !== big), self].map((p) => callTile(calls, p))}</div>
                </div>
            )
        } else {
            const pip = people.length <= 1
            const tiles = pip ? people : [...people, self]
            const cols = gridColumns(Math.max(tiles.length, 1), s.callFocus)
            stage = (
                <div class={pip ? 'meet-stage chat-call-grid is-pair' : 'meet-stage chat-call-grid'} style={`--cols:${cols}`}>
                    {people.length === 0 && (
                        <div class="chat-call-waiting">
                            {Avatar(calls.myRoom() ? calls.rooms()?.names[calls.myRoom() - 1] ?? name : name, 96)}
                            <span>
                                {calls.myRoom()
                                    ? `Nobody else is in ${calls.rooms()?.names[calls.myRoom() - 1] ?? 'this room'} yet`
                                    : a.since === null
                                      ? 'Ringing…'
                                      : others.length
                                        ? 'Waiting for their picture and sound…'
                                        : 'Nobody else is in the call yet'}
                            </span>
                        </div>
                    )}
                    {tiles.map((p) => callTile(calls, p))}
                    {pip && self.video && <div class="chat-call-pip">{callTile(calls, self)}</div>}
                </div>
            )
        }

        // ── Notices, each with its own button ────────────────────────────
        const toasts = (
            <div class="meet-toasts">
                {calls.toasts.map((t) => (
                    <div class={`meet-toast is-${t.kind}`} key={t.id} role="status">
                        <span class="meet-toast-icon">
                            {Icon(
                                {free: 'mic', hand: 'back_hand', held: 'mic_off', info: 'chat', knock: 'group', rec: 'videocam'}[t.kind],
                            )}
                        </span>
                        <span class="meet-toast-text">
                            <b>{t.text}</b>
                            {t.sub && <span>{t.sub}</span>}
                        </span>
                        {t.kind === 'hand' && hosting && t.participant !== undefined && (
                            <button
                                class="meet-btn is-primary"
                                onclick={() => {
                                    calls.control({AllowTalk: {participant: t.participant}})
                                    calls.dismissToast(t.id)
                                }}
                            >
                                Let talk
                            </button>
                        )}
                        {t.kind === 'knock' && hosting && t.participant !== undefined && [
                            <button class="meet-btn" onclick={() => calls.deny(t.participant!)} title="Turn away">
                                Turn away
                            </button>,
                            <button class="meet-btn is-primary" onclick={() => calls.admit(t.participant!)}>
                                Admit
                            </button>,
                        ]}
                        {t.kind === 'held' && !me?.hand && (
                            <button class="meet-btn is-primary" onclick={() => calls.raiseHand(true)}>
                                {Icon('back_hand')} Raise hand
                            </button>
                        )}
                        <button aria-label="Dismiss" class="meet-btn is-icon" onclick={() => calls.dismissToast(t.id)} title="Dismiss">
                            {Icon('close')}
                        </button>
                    </div>
                ))}
            </div>
        )
        const nudge = calls.nudging() && (
            <div class="meet-nudge" role="status">
                <span class="meet-toast-icon is-cream">{Icon('mic_off')}</span>
                <span class="meet-toast-text">
                    <b>You're muted</b>
                    <span>
                        You've been talking. Hold <kbd>Space</kbd> to talk.
                    </span>
                </span>
                <button class="meet-btn is-primary" onclick={() => calls.setMuted(false)}>
                    Unmute
                </button>
            </div>
        )
        const sharing = a.sharing && (
            <div class="meet-sharing" role="status">
                <i />
                <b>You're sharing your screen</b>
                <span>{others.length} watching</span>
                <button class="meet-btn is-danger" onclick={() => void calls.setScreen(false)}>
                    {Icon('stop_screen_share')} Stop
                </button>
            </div>
        )

        // ── The dock ─────────────────────────────────────────────────────
        const held = !!me?.held
        const raised = me?.hand != null
        const key = (icon: string, label: string, tone: string, onclick: () => void, extra: m.Children = null) => (
            <button aria-label={label} class={`meet-key ${tone}`} onclick={onclick} title={label}>
                {Icon(icon)}
                {extra}
            </button>
        )
        const react = menu === 'react' && (
            <div class="meet-pop is-react" role="menu">
                {CALL_REACTIONS.map((e) => (
                    <button
                        class="meet-emoji"
                        onclick={() => {
                            void calls.react(e)
                            s.callMenu = null
                        }}
                        title={`React ${e}`}
                    >
                        {e}
                    </button>
                ))}
            </div>
        )
        // An action closes its menu; a switch leaves it open, so several can be
        // set in a row. A row opening a submenu sets `callMenu` after this.
        const row = (icon: string, label: string, onclick: () => void, opts: {danger?: boolean; on?: boolean; hint?: string} = {}) => (
            <button
                class={opts.danger ? 'meet-menu-row is-danger' : 'meet-menu-row'}
                onclick={() => {
                    if (opts.on === undefined) s.callMenu = null
                    onclick()
                }}
                role="menuitem"
            >
                {Icon(icon)}
                <span>{label}</span>
                {opts.hint && <small>{opts.hint}</small>}
                {opts.on !== undefined && <span class={opts.on ? 'meet-switch is-on' : 'meet-switch'} />}
            </button>
        )
        const set = (patch: Partial<typeof settings>) => calls.control({Settings: {...settings, ...patch}})
        const more = menu === 'more' && (
            <div class="meet-pop is-menu" role="menu">
                <div class="meet-menu-label">Layout</div>
                {row('featured_video', 'Speaker', () => (s.callLayout = 'speaker'), {on: s.callLayout === 'speaker'})}
                {row('grid_view', 'Gallery', () => (s.callLayout = 'gallery'), {on: s.callLayout === 'gallery'})}
                <hr />
                <div class="meet-menu-label">This device</div>
                {row('mic', 'Microphone and camera…', () => {
                    s.callMenu = 'devices'
                    void this.loadDevices()
                })}
                {row('featured_video', 'Blur my background', () => void calls.setBlur(!a.blur), {on: a.blur})}
                {row('closed_caption', `Captions in ${captionLabel(calls.captionLang)}`, () => (s.callMenu = 'captions'))}
                {(settings.may_record || calls.recording()) &&
                    row('videocam', calls.recording() ? 'Stop recording and save' : 'Record on this device', () => {
                        void calls.setRecording(!calls.recording(), name)
                        s.callMenu = null
                    }, {hint: calls.recording() ? undefined : 'Everybody sees it'})}
                {hosting && [
                    <hr />,
                    <div class="meet-menu-label">Hosting</div>,
                    row('lock', 'Lock the call', () => set({locked: !settings.locked}), {on: settings.locked}),
                    row('group', 'Members wait to be let in', () => set({members_wait: !settings.members_wait}), {on: settings.members_wait}),
                    row('mic', 'Others may unmute', () => set({may_unmute: !settings.may_unmute}), {on: settings.may_unmute}),
                    row('screen_share', 'Others may share', () => set({may_share: !settings.may_share}), {on: settings.may_share}),
                    row('videocam', 'Anybody may record', () => set({may_record: !settings.may_record}), {on: settings.may_record}),
                    row('mic_off', 'Mute everyone', () => {
                        calls.control('MuteAll')
                        s.callMenu = null
                    }),
                    calls.rooms()
                        ? row('group', 'Bring everybody back', () => {
                              calls.closeRooms()
                              s.callMenu = null
                          })
                        : row('grid_view', 'Breakout rooms…', () => (s.callMenu = 'rooms')),
                    <hr />,
                    row('call_end', 'End for everyone', () => calls.control('EndForAll'), {danger: true}),
                ]}
            </div>
        )
        // The panels More opens onto: captions' language, devices, breakout rooms.
        const captionsMenu = menu === 'captions' && (
            <div class="meet-pop is-menu" role="menu">
                <div class="meet-menu-label">Captions of my voice</div>
                {CAPTION_LANGUAGES.map((l) =>
                    row('closed_caption', l.label, () => {
                        void calls.setCaptions(true, l.code)
                        s.callMenu = null
                    }, {on: calls.captioning() && calls.captionLang === l.code}),
                )}
                {calls.captioning() && row('close', 'Captions off', () => {
                    void calls.setCaptions(false)
                    s.callMenu = null
                })}
                <small class="meet-menu-note">Made on this device. Your voice goes to no speech service.</small>
            </div>
        )
        const devicesMenu = menu === 'devices' && (
            <div class="meet-pop is-menu is-devices" role="menu">
                <div class="meet-menu-label">Microphone</div>
                {s.devices.filter((d) => d.kind === 'audioinput').map((d) =>
                    row('mic', d.label || 'Microphone', () => void calls.setDevices({audio: d.deviceId}), {on: s.audioDevice === d.deviceId}),
                )}
                <div class="meet-menu-label">Camera</div>
                {s.devices.filter((d) => d.kind === 'videoinput').map((d) =>
                    row('videocam', d.label || 'Camera', () => {
                        s.videoDevice = d.deviceId
                        void calls.setDevices({video: d.deviceId})
                    }, {on: s.videoDevice === d.deviceId}),
                )}
            </div>
        )
        const roomsMenu = menu === 'rooms' && (
            <div class="meet-pop is-menu is-rooms" role="menu">
                <div class="meet-menu-label">Breakout rooms</div>
                <div class="meet-chips">
                    {[2, 3, 4, 5].map((n) => (
                        <button class={s.roomCount === n ? 'meet-chip is-on' : 'meet-chip'} onclick={() => (s.roomCount = n)}>
                            {n} rooms
                        </button>
                    ))}
                </div>
                <div class="meet-chips">
                    {[5, 10, 15, 0].map((mins) => (
                        <button class={s.roomMinutes === mins ? 'meet-chip is-on' : 'meet-chip'} onclick={() => (s.roomMinutes = mins)}>
                            {mins ? `${mins} min` : 'Until closed'}
                        </button>
                    ))}
                </div>
                {row('group', 'People may move between rooms', () => (s.roomMove = !s.roomMove), {on: s.roomMove})}
                <small class="meet-menu-note">Everybody but the hosts is shared out at random; move anybody afterwards from People.</small>
                <button
                    class="meet-btn is-primary meet-menu-go"
                    onclick={() => {
                        calls.openRooms(Array.from({length: s.roomCount}, (_, i) => `Room ${i + 1}`), s.roomMinutes, s.roomMove)
                        s.callMenu = null
                    }}
                >
                    Open {s.roomCount} rooms
                </button>
            </div>
        )
        const leave = () => {
            if (hosting && others.length) setMenu('leave')
            else calls.hangUp()
        }
        const leaveMenu = menu === 'leave' && (
            <div class="meet-pop is-menu is-leave" role="menu">
                <div class="meet-menu-label">Leave the call?</div>
                {row('logout', 'Leave', () => calls.hangUp(), {hint: 'The call goes on'})}
                {row('call_end', 'End for everyone', () => calls.control('EndForAll'), {danger: true, hint: `${others.length} others leave too`})}
            </div>
        )
        const dock = (
            <footer class="meet-dock">
                {key(held ? 'lock' : a.muted ? 'mic_off' : 'mic', held ? 'A host muted you' : a.muted ? 'Unmute (M)' : 'Mute (M)', a.muted || held ? 'is-off' : '', () => calls.setMuted(!a.muted))}
                {key(a.camera ? 'videocam' : 'videocam_off', a.camera ? 'Camera off (V)' : 'Camera on (V)', a.camera ? '' : 'is-off', () => void calls.setCamera(!a.camera))}
                <span class="meet-sep" />
                {key(a.sharing ? 'stop_screen_share' : 'screen_share', a.sharing ? 'Stop sharing' : 'Share your screen', a.sharing ? 'is-on' : '', () => void calls.setScreen(!a.sharing))}
                <span class="meet-anchor">
                    {key('add_reaction', 'React', menu === 'react' ? 'is-sel' : '', () => setMenu('react'))}
                    {react}
                </span>
                {key('back_hand', raised ? 'Lower your hand (H)' : 'Raise your hand (H)', raised ? 'is-hand' : '', () => calls.raiseHand(!raised))}
                {key('closed_caption', calls.captioning() ? 'Captions off (C)' : 'Captions on (C)', calls.captioning() ? 'is-sel' : '', () =>
                    void calls.setCaptions(!calls.captioning()),
                )}
                <span class="meet-sep" />
                <span class="meet-anchor">
                    {key('more_horiz', 'More', ['more', 'captions', 'devices', 'rooms'].includes(menu ?? '') ? 'is-sel' : '', () => setMenu('more'))}
                    {more}
                    {captionsMenu}
                    {devicesMenu}
                    {roomsMenu}
                </span>
                <span class="meet-sep" />
                <span class="meet-anchor">
                    <button class="meet-leave" onclick={leave} title="Leave the call">
                        {Icon('call_end')}
                        <span>Leave</span>
                    </button>
                    {leaveMenu}
                </span>
            </footer>
        )

        // ── People ───────────────────────────────────────────────────────
        const members = calls.members()
        const hands = members.filter((m) => m.hand !== null).sort((x, y) => (x.hand ?? 0) - (y.hand ?? 0))
        const panel = s.callFocus && s.callPanel === 'people' && (
            <aside class="meet-panel" aria-label="People in the call">
                <header class="meet-panel-head">
                    <b>People · {members.length}</b>
                    <button aria-label="Close" class="meet-btn is-icon" onclick={() => (s.callPanel = null)} title="Close">
                        {Icon('close')}
                    </button>
                </header>
                <div class="meet-panel-body">
                    {hosting && calls.waiting().length > 0 && (
                        <section class="meet-knock">
                            <h4>
                                Waiting to join · {calls.waiting().length}
                                {calls.waiting().length > 1 && (
                                    <button class="meet-btn is-gold" onclick={() => calls.admitAll()}>
                                        Admit all
                                    </button>
                                )}
                            </h4>
                            {calls.waiting().map((w) => (
                                <div class="meet-row" key={`w${w.participant}`}>
                                    {Avatar(w.username, 30)}
                                    <span class="meet-row-name">
                                        <b>
                                            {w.username}
                                            {w.guest && <span class="meet-badge is-guest">GUEST</span>}
                                        </b>
                                        <small>waiting {waitedFor(w.since_ms)}</small>
                                    </span>
                                    <button aria-label="Turn away" class="meet-btn is-icon" onclick={() => calls.deny(w.participant)} title="Turn away">
                                        {Icon('close')}
                                    </button>
                                    <button aria-label="Admit" class="meet-btn is-primary" onclick={() => calls.admit(w.participant)} title="Admit">
                                        {Icon('check')}
                                    </button>
                                </div>
                            ))}
                        </section>
                    )}
                    {hands.length > 0 && (
                        <section>
                            <h4>Hands up <span>in order</span></h4>
                            {hands.map((mem, i) => (
                                <div class="meet-row" key={`h${mem.participant}`}>
                                    <span class="meet-q">{i + 1}</span>
                                    <span class="meet-row-name">
                                        <b>{mem.participant === a.participant ? 'You' : mem.username}</b>
                                    </span>
                                    {hosting && (
                                        <button class="meet-btn is-primary" onclick={() => calls.control({AllowTalk: {participant: mem.participant}})}>
                                            Let talk
                                        </button>
                                    )}
                                    {(hosting || mem.participant === a.participant) && (
                                        <button
                                            aria-label="Lower"
                                            class="meet-btn is-icon"
                                            onclick={() =>
                                                mem.participant === a.participant ? calls.raiseHand(false) : calls.control({LowerHand: {participant: mem.participant}})
                                            }
                                            title="Lower the hand"
                                        >
                                            {Icon('close')}
                                        </button>
                                    )}
                                </div>
                            ))}
                        </section>
                    )}
                    <section>
                        <h4>In the call <span>{members.length}</span></h4>
                        {members.map((mem) => {
                            const isMe = mem.participant === a.participant
                            const id = `person${mem.participant}`
                            return (
                                <div class={calls.speaking(isMe ? -1 : mem.participant) ? 'meet-row is-talking' : 'meet-row'} key={`m${mem.participant}`}>
                                    {Avatar(mem.username, 30)}
                                    <span class="meet-row-name">
                                        <b>
                                            {isMe ? 'You' : mem.username}
                                            {mem.role !== 'member' && <span class="meet-badge">{mem.role === 'host' ? 'HOST' : 'CO-HOST'}</span>}
                                        </b>
                                        {mem.sharing && <small>presenting</small>}
                                    </span>
                                    <span class="meet-row-state">
                                        {mem.sharing && Icon('screen_share')}
                                        {mem.held ? Icon('lock') : mem.muted ? <span class="is-muted">{Icon('mic_off')}</span> : Icon('mic')}
                                    </span>
                                    {hosting && !isMe && (
                                        <span class="meet-anchor">
                                            <button aria-label="Host actions" class="meet-btn is-icon" onclick={() => setMenu(id)} title="Host actions">
                                                {Icon('more_horiz')}
                                            </button>
                                            {menu === id && (
                                                <div class="meet-pop is-menu is-person" role="menu">
                                                    {mem.held
                                                        ? row('mic', 'Let talk', () => calls.control({AllowTalk: {participant: mem.participant}}))
                                                        : row('mic_off', 'Mute', () => calls.control({Mute: {participant: mem.participant}}))}
                                                    {mem.sharing && row('stop_screen_share', 'Stop their screen', () => calls.control({StopSharing: {participant: mem.participant}}))}
                                                    {mem.role === 'member' && row('shield_person', 'Make co-host', () => calls.control({Role: {participant: mem.participant, role: 'CoHost'}}))}
                                                    {mem.role === 'cohost' && row('shield_person', 'Make a member again', () => calls.control({Role: {participant: mem.participant, role: 'Member'}}))}
                                                    {me?.role === 'host' && mem.role !== 'host' && row('star', 'Hand the call over', () => calls.control({Role: {participant: mem.participant, role: 'Host'}}))}
                                                    {calls.spotlight() === mem.participant
                                                        ? row('featured_video', 'Stop the spotlight', () => calls.setSpotlight(null))
                                                        : row('featured_video', 'Spotlight for everyone', () => calls.setSpotlight(mem.participant))}
                                                    {calls.rooms() &&
                                                        ['the main call', ...calls.rooms()!.names].map((room, i) =>
                                                            i === mem.room ? null : row('group', `Move to ${room}`, () => calls.moveTo(mem.participant, i)),
                                                        )}
                                                    <hr />
                                                    {row('person_remove', 'Remove from the call', () => calls.control({Remove: {participant: mem.participant}}), {danger: true})}
                                                </div>
                                            )}
                                        </span>
                                    )}
                                </div>
                            )
                        })}
                    </section>
                </div>
                {hosting && (
                    <footer class="meet-panel-foot">
                        <button class="meet-btn" onclick={() => calls.control('MuteAll')}>
                            {Icon('mic_off')} Mute everyone
                        </button>
                    </footer>
                )}
            </aside>
        )

        return (
            <aside class={s.callFocus ? 'chat-call meet is-focus' : 'chat-call meet'}>
                <header class="chat-call-head">
                    <button class="chat-call-title" onclick={() => this.open(a.channel)} title="Open this chat">
                        <b>{name}</b>
                        <span class="chat-sub">
                            {Icon('lock')} Encrypted · {callStatus(calls)}
                        </span>
                    </button>
                    <button
                        class={s.callPanel === 'people' && s.callFocus ? 'chat-call-swap is-on' : 'chat-call-swap'}
                        onclick={() => {
                            s.callFocus = true
                            s.callPanel = s.callPanel === 'people' ? null : 'people'
                        }}
                        title="People in the call"
                    >
                        {Icon('group')}
                        <span>{members.length}</span>
                        {/* Raised hands, and for a host who knocks: what wants an answer. */}
                        {hands.length + (hosting ? calls.waiting().length : 0) > 0 && (
                            <span class="meet-hands">{hands.length + (hosting ? calls.waiting().length : 0)}</span>
                        )}
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
                <div class="meet-body">
                    <div class="meet-main">
                        {sharing}
                        {this.meetingChips(name)}
                        {stage}
                        {this.captionsOverlay()}
                        {toasts}
                        {nudge}
                        {dock}
                        {menu && <div class="meet-scrim" onclick={() => (s.callMenu = null)} />}
                    </div>
                    {panel}
                </div>
            </aside>
        )
    }

    /** Waiting for a host to let this page in: your own picture, who you wait
     *  for, and the way out. */
    waitingRoom(channel: string) {
        const calls = chat().calls
        const self = calls.selfView()
        return (
            <aside class={this.s.callFocus ? 'chat-call meet is-focus' : 'chat-call meet'}>
                <div class="meet-waiting">
                    {self ? <div class="meet-waiting-self">{Media(self, 'video', true)}</div> : Avatar(chat().address ?? 'You', 96)}
                    <h3>{this.chatTitle(channel)}</h3>
                    <p class="meet-waiting-state">
                        <span class="meet-spinner" />
                        Waiting for a host to let you in
                    </p>
                    <p class="chat-sub">
                        {Icon('lock')} End-to-end encrypted. You will see and hear the call once you are in.
                    </p>
                    <button class="meet-leave" onclick={() => calls.hangUp()}>
                        {Icon('call_end')}
                        <span>Stop waiting</span>
                    </button>
                </div>
            </aside>
        )
    }

    /** What the meeting says about everybody, over the stage: who records,
     *  and the breakout room this page is in, with its time and the way back. */
    meetingChips(title: string) {
        const calls = chat().calls
        const recorders = calls.members().filter((m) => m.recording)
        const rooms = calls.rooms()
        const room = calls.myRoom()
        const left = rooms?.until_ms ? Math.max(0, Math.round((rooms.until_ms - Date.now()) / 1000)) : null
        return (
            <div class="meet-chips-row">
                {recorders.length > 0 && (
                    <span class="meet-rec" title="Recording on their own device">
                        <i /> REC · {recorders.map((m) => (m.participant === calls.self()?.participant ? 'You' : m.username)).join(', ')}
                        {calls.recording() && (
                            <button class="meet-btn" onclick={() => void calls.setRecording(false, title)}>
                                Stop and save
                            </button>
                        )}
                    </span>
                )}
                {rooms && room > 0 && (
                    <span class="meet-room">
                        {Icon('group')} In {rooms.names[room - 1] ?? 'a breakout room'}
                        {left !== null && ` · ${Math.floor(left / 60)}:${String(left % 60).padStart(2, '0')} left`}
                        {rooms.may_move && (
                            <button class="meet-btn" onclick={() => calls.moveTo(calls.self()!.participant, 0)}>
                                Back to the call
                            </button>
                        )}
                    </span>
                )}
                {rooms && room === 0 && calls.hosting() && (
                    <span class="meet-room">
                        {Icon('group')} {rooms.names.length} rooms open
                        {left !== null && ` · ${Math.floor(left / 60)}:${String(left % 60).padStart(2, '0')} left`}
                        <button class="meet-btn" onclick={() => calls.closeRooms()}>
                            Bring everybody back
                        </button>
                    </span>
                )}
            </div>
        )
    }

    /** The newest line of captions, with whose voice it is. */
    captionsOverlay() {
        const c = chat().calls.captionNow()
        if (!c) return null
        return (
            <div class="meet-caption" aria-live="polite">
                {Avatar(c.who === 'You' ? (chat().address ?? 'You') : c.who, 28)}
                <span>
                    <b>{c.who}</b>
                    {c.text}
                </span>
            </div>
        )
    }

    /** The microphones and cameras this browser has, named once allowed. */
    async loadDevices() {
        try {
            this.s.devices = (await navigator.mediaDevices.enumerateDevices()).filter((d) => d.deviceId)
        } catch {
            this.s.devices = []
        }
        m.redraw()
    }

    /** On a phone the call takes the screen; folded, it is this green bar at the top. */
    callBar() {
        const calls = chat().calls
        // Only the bar shows outside Community: no camera is drawn, so none is sent.
        if (mode() !== 'community') calls.watch([])
        return (
            <button
                class="chat-call-bar"
                onclick={() => {
                    // From World or Admin, the call is back in Community.
                    if (mode() !== 'community') m.route.set('/')
                    this.focusCall(true)
                }}
            >
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

    communityPane(id: string) {
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
                    {Tabs(id, community.topics.length)}
                    {tabOf(id) === 'posts' ? Posts(community, me ?? '', {
                        avatar: (who, size) => Avatar(who, size),
                        // A post's place: World, flown to the view it was written from.
                        onMap: (place) => {
                            this.postPlace = place
                            m.route.set(PATHS.world)
                        },
                        pictureUrl,
                    }) : <>
                    <p class="chat-hint">Members only, end-to-end encrypted. Never on the fediverse.</p>
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
                    </>}
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
