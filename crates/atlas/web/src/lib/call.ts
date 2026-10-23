/**
 * Calls on the chat page (`docs/notes/calls.md`, "Calls in the browser" and "A
 * call is a meeting").
 *
 * The media is this page's: `RTCPeerConnection` and `getUserMedia` exist in a
 * page and not in the chat worker. So the page runs the call (`webcall.js`,
 * the same module the viewer uses) and the worker, which owns the connection
 * and the keys, carries the signalling and hands over the chat's call key for
 * each epoch. One call at a time, in the tab that placed or answered it; other
 * tabs only see it ring.
 *
 * The rules are the app's (`systems::calls`): a roster without this account in
 * it is the call ringing, an empty one is the call over, nobody answering for a
 * minute ends a call placed here, and a one-to-one call ends when the other
 * person declines or leaves.
 *
 * Since protocol 41 a call is a meeting: its roster says who hosts, whose hand
 * is up and in what order, who is muted or held, who shares a screen. Host
 * actions go to the server, which carries them out (`CallControl`); reactions
 * are sealed here (`call-events.js`) and relayed unread. Who is talking is
 * worked out on this page from the sound it already plays, so the server learns
 * nothing from it.
 */
import {guestIndex, openEvent, openGuestKey, sealEvent, sealGuestKey} from '../../../../cartopolis/web/call-events.js'
import {WebCall, webCallSupported} from '../../../../cartopolis/web/webcall.js'

import type {ChatClient} from './chat'
import {CallTones} from './call-tones'
import {CALL_TAG, closeCallNotifications} from './web-push'
import {CallRecorder} from './call-recorder'
import {Captioner} from './call-captions'
import {buildRecord} from './meeting'

/** The call worker's content-hashed name, put in by `build.ts`. */
declare const __CALL_WORKER_URL__: string | undefined
const CALL_WORKER_URL = typeof __CALL_WORKER_URL__ === 'string' ? __CALL_WORKER_URL__ : '/call-worker.js'

export type CallRole = 'host' | 'cohost' | 'member'

/** Somebody in a chat's call. `username` is the bare name, as the server keeps it. */
export interface CallMember {
    device: string
    /** A raised hand's place in the queue: lower went up first. */
    hand: number | null
    /** A host muted them, and they may not unmute until let talk. */
    held: boolean
    muted: boolean
    participant: number
    role: CallRole
    sharing: boolean
    username: string
    /** Recording the call on their own device. */
    recording: boolean
    /** A guest, let in by an invite link. */
    guest: boolean
    /** Their breakout room; 0 is the main call. */
    room: number
    /** An AI agent speaks for the account (protocol 43). */
    agent: boolean
}

/** Somebody waiting to be let in. */
export interface CallWaiter {
    guest: boolean
    participant: number
    since_ms: number
    username: string
}

/** Open breakout rooms: room `n` is `names[n - 1]`. */
export interface CallRooms {
    may_move: boolean
    names: string[]
    until_ms: number | null
}

/** What a call's hosts allow everybody else. */
export interface CallSettings {
    locked: boolean
    may_share: boolean
    may_unmute: boolean
    members_wait: boolean
    may_record: boolean
}

export interface Tile {
    audio: MediaStream | null
    participant: number
    screen: MediaStream | null
    video: MediaStream | null
}

/** A reaction floating over somebody's tile; `participant` -1 is this device. */
export interface Reaction {
    at: number
    emoji: string
    id: number
    participant: number
}

/** Something that happened in the call, with what can be done about it. */
export interface Toast {
    at: number
    id: number
    kind: 'hand' | 'held' | 'info' | 'free' | 'knock' | 'rec'
    participant?: number
    sub?: string
    text: string
}

/** The keys of the chat's current epoch, as bytes. */
interface Keys {
    epoch: number
    key: Uint8Array
}

interface Active {
    channel: string
    /** Whether the connection to the forwarder is up. */
    connected: boolean
    /** When somebody else was first there with it up: what the timer counts from. */
    since: number | null
    /** When this device placed or joined it, for the no-answer rule. */
    started: number
    web: WebCall
    epoch: number | null
    keys: Keys | null
    /** The epoch before, for a reaction sealed just before a change. */
    lastKeys: Keys | null
    camera: boolean
    muted: boolean
    /** What the call was last told about the microphone. */
    toldMuted: boolean | null
    sharing: boolean
    participant: number | null
    tiles: Map<number, Tile>
    keyTimer: ReturnType<typeof setInterval>
    /** Who is heard, by participant, for who is talking. */
    listeners: Map<number, {analyser: AnalyserNode; source: MediaStreamAudioSourceNode}>
    audio: AudioContext | null
    /** This device's own microphone, never muted, to catch talking while muted. */
    probe: AnalyserNode | null
    levelTimer: ReturnType<typeof setInterval>
    /** When each participant was last heard above the speaking level, ms. */
    heard: Map<number, number>
    /** The person on the big tile: the one talking, kept until somebody else talks a while. */
    speaker: number | null
    /** Who has been talking since when, to switch the big tile only after a while. */
    candidate: {participant: number; since: number} | null
    /** Talking into a muted microphone, since when. */
    mutedTalk: number | null
    /** Show the "you're muted" nudge. */
    nudge: boolean
    /** Waiting for a host to let this device in. */
    waiting: boolean
    recorder: CallRecorder | null
    captioner: Captioner | null
    /** The newest line of captions per person (`-1`: this device). */
    captions: Map<number, {at: number; final: boolean; text: string}>
    blur: boolean
    /** Whose cameras the forwarder was last told this page draws. */
    watching: string
    /** Everybody who was in the call while this device was, for the record. */
    seen: Set<string>
    /** Where the meeting is on its plan's agenda: the item, since when.
     *  Kept by a host's device and sent sealed to everybody. */
    agenda: {index: number; plan: string; startedMs: number} | null
    /** This device is ending it for everybody: it writes the record. */
    endingForAll: boolean
    /** How much the last record posted from here held, so ending does not
     *  post the same one twice. */
    recorded: number
    /** This page is a guest: the link's secret, which opens the key a host
     *  hands it. A guest is in no chat, so it has no key of its own. */
    guestSecret: Uint8Array | null
    /** Hosting: the epoch each guest in the call was last handed, by participant. */
    dropped: Map<number, number>
}

/** The channel's group id, as the bytes a guest's key is bound to. */
const groupBytes = (channel: string) => unhexBytes(channel.replace(/^dm:/, ''))
const unhexBytes = (s: string) => new Uint8Array((s.match(/../g) ?? []).map((h) => parseInt(h, 16)))
const b64url = (b: Uint8Array) => btoa(String.fromCharCode(...b)).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '')
const unb64url = (s: string) => Uint8Array.from(atob(s.replace(/-/g, '+').replace(/_/g, '/')), (c) => c.charCodeAt(0))

/** A guest link in a chat line: its code, and the secret after `#`. */
export const GUEST_LINK = /[?&]meet=([a-z0-9]{10})#([A-Za-z0-9_-]{22})/

/** How a person in a call is named on screen: a guest by the name they gave
 *  (their account name, `Sanne-Jansen~3fa`, is made unique) and marked as one. */
export function callName(m: {guest: boolean; username: string}): string {
    return m.guest ? `${m.username.replace(/~[0-9a-f]{3}$/, '').replace(/-/g, ' ')} (guest)` : m.username
}

/** A guest link's code and secret, from a URL or a line that holds one. */
export function readGuestLink(text: string): {code: string; secret: Uint8Array} | null {
    const m = GUEST_LINK.exec(text)
    return m ? {code: m[1], secret: unb64url(m[2])} : null
}

/** How long a call placed here rings before giving up, ms. */
const NO_ANSWER_MS = 60_000
/** Above this RMS (full scale 1) somebody is talking: the app's `SPEAKING_RMS`. */
const SPEAKING_RMS = 0.02
/** How long a tile stays lit after the last loud moment, ms. */
const SPEAKING_HOLD_MS = 400
/** How long somebody talks before the big tile moves to them, ms. */
const SPEAKER_SWITCH_MS = 1500
/** How long talking into a muted microphone before the nudge, ms. */
const NUDGE_AFTER_MS = 1500
/** How long a reaction floats, ms. */
export const REACTION_MS = 4000
/** How long a notice stays on its own, ms. */
const TOAST_MS = 8000

let nextId = 1

const hex = (b: Uint8Array) => Array.from(b, (x) => x.toString(16).padStart(2, '0')).join('')
const unhex = (s: string) => new Uint8Array((s.match(/../g) ?? []).map((h) => parseInt(h, 16)))

function rms(a: AnalyserNode, buf: Float32Array<ArrayBuffer>): number {
    a.getFloatTimeDomainData(buf)
    let sum = 0
    for (const v of buf) sum += v * v
    return Math.sqrt(sum / buf.length)
}

export class Calls {
    active: Active | null = null
    /** Every chat's call, by channel. */
    readonly rosters = new Map<string, CallMember[]>()
    /** What each call's hosts allow, by channel. */
    readonly settingsOf = new Map<string, CallSettings>()
    /** Who waits to be let in, by channel. */
    readonly waitingOf = new Map<string, CallWaiter[]>()
    /** Who the hosts show large for everybody, by channel. */
    readonly spotlightOf = new Map<string, number | null>()
    /** The open breakout rooms, by channel. */
    readonly roomsOf = new Map<string, CallRooms | null>()
    /** Captions on, and in which language this device speaks. */
    captionLang = typeof navigator === 'undefined' ? 'en-US' : navigator.language.startsWith('nl') ? 'nl-NL' : 'en-US'
    /** What was said, finished lines only, for the meeting record. */
    transcript: {at: number; text: string; who: string}[] = []
    /** Calls this person waved away, so they stop ringing. */
    readonly dismissed = new Set<string>()
    /** Something to tell the person about a call. */
    notice: string | null = null
    /** Notices inside the call, newest last. */
    toasts: Toast[] = []
    /** Reactions in the call, newest last. */
    reactions: Reaction[] = []
    /** This page came in by a guest link: the link, the name given, and the
     *  call it may join once the server says which (`call_invited`). */
    guest: {camera: boolean; channel: string | null; name: string; secret: Uint8Array} | null = null
    /** The newest guest link made on this page, to show and copy. */
    invited: {channel: string; url: string} | null = null
    /** Guest links by channel: made here, or read from the chat they were
     *  posted in, so any host's page can hand a guest the key. */
    private links = new Map<string, Uint8Array>()
    /** Links asked for and not answered yet: their secrets, by channel. */
    private asking = new Map<string, Uint8Array>()

    private client: ChatClient
    private changed: () => void
    private tones = new CallTones()
    /** The browser notification for the call ringing now, while this tab is not looked at. */
    private ringNote: {channel: string; note: Notification} | null = null
    private rangBefore = false

    constructor(client: ChatClient, changed: () => void) {
        this.client = client
        this.changed = changed
        addEventListener('pagehide', () => this.hangUp())
    }

    /** Every change in a call: its tones follow it, then the page redraws. */
    private notify(): void {
        const a = this.active
        this.tones.update(this.ringing() !== null, a !== null, a ? this.others(a.channel).length : 0)
        this.ringNotice()
        this.changed()
    }

    /**
     * A call ringing while this tab is in the background, or another window is in
     * front, is a browser notification as well as the dialog: the ringtone alone
     * cannot say who. Only once the person has allowed notifications (the page
     * asks on the first click, as for messages), only in a tab that is open, and
     * gone the moment the call stops ringing. It says who is calling, which only
     * this page and the person ever see.
     */
    private ringNotice(): void {
        const channel = this.ringing()
        // Stopped ringing: the "Incoming call" a push put up (`sw.js`) goes too.
        if (this.rangBefore && !channel) closeCallNotifications()
        this.rangBefore = channel !== null
        if (this.ringNote && this.ringNote.channel !== channel) {
            this.ringNote.note.close()
            this.ringNote = null
        }
        if (!channel || this.ringNote || (!document.hidden && document.hasFocus())) return
        if (typeof Notification === 'undefined' || Notification.permission !== 'granted') return
        const who = this.others(channel)[0]?.username ?? 'Somebody'
        const note = new Notification('Incoming call', {body: `${who} is calling`, requireInteraction: true, tag: CALL_TAG})
        note.onclick = () => {
            window.focus()
            this.client.open(channel)
            note.close()
        }
        this.ringNote = {channel, note}
    }

    static supported(): boolean {
        return webCallSupported()
    }

    /** The bare username this page is signed in as. */
    private me(): string {
        return (this.client.address ?? '').split('@')[0].toLowerCase()
    }

    /** Who is in a chat's call, besides this person. */
    others(channel: string): CallMember[] {
        return (this.rosters.get(channel) ?? []).filter((m) => m.username.toLowerCase() !== this.me())
    }

    /** Everybody in this page's call, this device included, in join order. */
    members(): CallMember[] {
        return this.active ? (this.rosters.get(this.active.channel) ?? []) : []
    }

    /** This device in its call, as the roster has it. */
    self(): CallMember | null {
        const a = this.active
        return a && a.participant !== null ? (this.members().find((m) => m.participant === a.participant) ?? null) : null
    }

    /** Whether this device may take a host's actions. */
    hosting(): boolean {
        const role = this.self()?.role
        return role === 'host' || role === 'cohost'
    }

    /** What the hosts of this page's call allow. */
    settings(): CallSettings {
        return (
            (this.active && this.settingsOf.get(this.active.channel)) || {
                locked: false,
                may_record: false,
                may_share: true,
                may_unmute: true,
                members_wait: false,
            }
        )
    }

    /** The person whose shared screen is showing: the latest sharer with a picture. */
    sharer(): {member: CallMember; stream: MediaStream} | null {
        const a = this.active
        if (!a) return null
        for (const m of [...this.members()].reverse()) {
            if (!m.sharing || m.participant === a.participant) continue
            const stream = a.tiles.get(m.participant)?.screen
            if (stream) return {member: m, stream}
        }
        return null
    }

    /** Whether `participant` is talking now; `-1` is this device. */
    speaking(participant: number): boolean {
        const a = this.active
        if (!a) return false
        const at = a.heard.get(participant)
        return at !== undefined && Date.now() - at < SPEAKING_HOLD_MS
    }

    /** The person for the big tile in Speaker view. */
    speaker(): number | null {
        return this.active?.speaker ?? null
    }

    /** The reaction floating over a tile now, if any; `-1` is this device. */
    reactionOf(participant: number): Reaction | null {
        const now = Date.now()
        for (let i = this.reactions.length - 1; i >= 0; i--) {
            const r = this.reactions[i]
            if (r.participant === participant && now - r.at < REACTION_MS) return r
        }
        return null
    }

    /** A chat whose call is ringing here, if any. */
    ringing(): string | null {
        for (const [channel, members] of this.rosters) {
            if (!members.length || this.dismissed.has(channel) || this.active?.channel === channel) continue
            if (!members.some((m) => m.username.toLowerCase() === this.me())) return channel
        }
        return null
    }

    private toast(t: Omit<Toast, 'at' | 'id'>): void {
        // One per person and kind (a hand raised twice is one notice), and one
        // of each about this device: being held, or let talk, replace each other.
        const mine = (k: Toast['kind']) => k === 'held' || k === 'free'
        this.toasts = this.toasts.filter((x) =>
            mine(t.kind) ? !mine(x.kind) : !(x.kind === t.kind && x.participant !== undefined && x.participant === t.participant),
        )
        this.toasts.push({...t, at: Date.now(), id: nextId++})
        if (this.toasts.length > 4) this.toasts.shift()
    }

    dismissToast(id: number): void {
        this.toasts = this.toasts.filter((t) => t.id !== id)
        this.notify()
    }

    /** An event from the chat worker (`call_*`). */
    onEvent(event: {kind: string; [k: string]: unknown}): void {
        const a = this.active
        switch (event.kind) {
            case 'call_roster': {
                const channel = event.channel as string
                const members = event.members as CallMember[]
                const before = this.rosters.get(channel) ?? []
                this.rosters.set(channel, members)
                if (event.settings) this.settingsOf.set(channel, event.settings as CallSettings)
                const waitingBefore = this.waitingOf.get(channel) ?? []
                this.waitingOf.set(channel, (event.waiting as CallWaiter[] | undefined) ?? [])
                this.spotlightOf.set(channel, (event.spotlight as number | null | undefined) ?? null)
                this.roomsOf.set(channel, (event.rooms as CallRooms | null | undefined) ?? null)
                if (a && a.channel === channel) this.knocks(waitingBefore, this.waitingOf.get(channel) ?? [])
                if (!members.length) {
                    // Over. A call that rang here and nobody here touched is missed.
                    if (before.length && !this.dismissed.has(channel) && a?.channel !== channel) {
                        const who = before[0]?.username
                        this.notice = who ? `Missed call from ${who}` : 'Missed call'
                    }
                    this.dismissed.delete(channel)
                    this.settingsOf.delete(channel)
                }
                if (a && a.channel === channel) {
                    a.web.forgetGone()
                    for (const m of members) if (!a.waiting) a.seen.add(m.username)
                    this.meetingChanges(before, members)
                    // A guest just let in gets the key from whichever host sees it first.
                    void this.dropKeys()
                    const others = this.others(channel)
                    if (others.length && a.connected && a.since === null) a.since = Date.now()
                    // The last other person hung up: this side ends too, as a phone does.
                    if (!others.length && a.since !== null) this.hangUp('The call ended')
                }
                break
            }
            case 'call_joined':
                if (a && a.channel === event.channel) {
                    a.participant = event.participant as number
                    a.waiting = false
                    void a.web.joined(event.answer as string)
                    this.tellMuted()
                }
                break
            case 'call_waiting':
                if (a && a.channel === event.channel) {
                    a.participant = event.participant as number
                    a.waiting = true
                }
                break
            case 'call_offer':
                if (a && a.channel === event.channel) {
                    void a.web.offered(
                        event.sdp as string,
                        event.tracks as {kind: 'audio' | 'camera' | 'screen'; mid: string; participant: number; video: boolean}[],
                    )
                }
                break
            case 'call_declined': {
                const channel = event.channel as string
                const who = (event.username as string).toLowerCase()
                if (who === this.me()) {
                    // Another of this person's devices declined: stop ringing here too.
                    this.dismissed.add(channel)
                } else if (a && a.channel === channel && !this.others(channel).length) {
                    const group = this.client.chats.find((c) => c.channel === channel)?.group
                    this.notice = `${event.username} declined`
                    if (!group) this.hangUp()
                }
                break
            }
            case 'call_refused':
                // A refused host action: said, and the call goes on.
                if (event.control) {
                    this.toast({kind: 'info', text: event.message as string})
                    break
                }
                this.notice = `The call could not start: ${event.message}`
                this.hangUp()
                break
            case 'call_ended':
                if (a && a.channel === event.channel) {
                    const by = event.by as string
                    this.close(event.reason === 'removed' ? `${by} removed you from the call` : `${by} ended the call for everyone`)
                }
                break
            case 'call_reacted':
                if (a && a.channel === event.channel) void this.reacted(event.participant as number, event.epoch as string, event.sealed as string)
                return
            case 'call_invited': {
                const channel = event.channel as string
                const code = event.code as string
                // No code: this page is a guest, being told which call it may join.
                if (!code) {
                    if (this.guest && !this.guest.channel) {
                        this.guest.channel = channel
                        void this.join(channel, this.guest.camera)
                    }
                    break
                }
                const secret = this.asking.get(channel)
                if (!secret) break
                this.asking.delete(channel)
                this.links.set(channel, secret)
                const url = `${location.origin}/?meet=${code}#${b64url(secret)}`
                this.invited = {channel, url}
                // Posted in the chat, sealed: every member's page then knows the
                // secret, so whichever host lets the guest in can hand it the key.
                this.client.sendText(channel, `Guest link for the call, good for a day: ${url}`)
                void navigator.clipboard?.writeText(url).catch(() => undefined)
                this.toast({kind: 'info', sub: 'Posted in the chat, and copied', text: 'Guest link ready'})
                break
            }
            case 'call_key_dropped':
                if (a && a.channel === event.channel && a.guestSecret) void this.keyDropped(a.guestSecret, event.sealed as string)
                return
            default:
                return
        }
        this.notify()
    }

    /** What changed in the meeting since the last roster, worth a notice. */
    private meetingChanges(before: CallMember[], now: CallMember[]): void {
        const a = this.active
        if (!a) return
        const was = new Map(before.map((m) => [m.participant, m]))
        const me = now.find((m) => m.participant === a.participant)
        const hosting = me?.role === 'host' || me?.role === 'cohost'
        for (const m of now) {
            const old = was.get(m.participant)
            if (m.participant === a.participant) {
                if (m.held && !old?.held) {
                    a.muted = true
                    a.web.setMuted(true)
                    this.toast({kind: 'held', sub: 'Raise your hand to ask to talk.', text: 'A host muted you'})
                } else if (!m.held && old?.held) {
                    this.toast({kind: 'free', sub: 'Unmute when you are ready.', text: 'You may talk now'})
                }
                continue
            }
            // Somebody starting to record is told to everyone, every time.
            if (m.recording && !old?.recording) {
                this.toast({kind: 'rec', participant: m.participant, sub: 'On their own device', text: `${callName(m)} is recording`})
            }
            // A hand going up is news to whoever runs the call.
            if (hosting && m.hand !== null && (old?.hand ?? null) === null) {
                this.toast({kind: 'hand', participant: m.participant, sub: 'In the queue', text: `${callName(m)} raised a hand`})
            }
        }
        // A hand that went down takes its notice with it.
        this.toasts = this.toasts.filter((t) => t.kind !== 'hand' || now.some((m) => m.participant === t.participant && m.hand !== null))
        // Somebody new hears where the agenda is from whoever keeps it.
        if (hosting && a.agenda && now.some((m) => !was.has(m.participant))) this.sendAgenda()
    }

    /** Place or join the call in `channel`, with the microphone, camera, blur
     *  and mute the check before joining chose. */
    async join(
        channel: string,
        camera = false,
        prefs: {audio?: string | null; blur?: boolean; muted?: boolean; video?: string | null} = {},
    ): Promise<void> {
        if (!Calls.supported()) {
            this.notice = 'This browser cannot make an encrypted call. Try a recent Chrome, Edge, Firefox or Safari.'
            this.notify()
            return
        }
        // A guest has no key of its own: a host hands it one once it is let in.
        const guest = this.guest?.channel === channel ? this.guest : null
        const keys = guest ? null : await this.client.callKeys(channel)
        if (!keys && !guest) {
            this.notice = 'This chat is not ready for a call yet.'
            this.notify()
            return
        }
        if (this.active) this.hangUp()
        this.dismissed.delete(channel)
        this.toasts = []
        this.reactions = []
        const tiles = new Map<number, Tile>()
        const web = new WebCall((kind: string, value: string) => this.onWeb(channel, kind, value), {
            onStream: (participant: number, kind: 'audio' | 'video' | 'screen', stream: MediaStream | null) => {
                const t = tiles.get(participant) ?? {audio: null, participant, screen: null, video: null}
                t[kind] = stream
                if (t.audio || t.video || t.screen) tiles.set(participant, t)
                else tiles.delete(participant)
                if (kind === 'audio') this.listen(participant, stream)
                this.notify()
            },
            screen: true,
            ...(prefs.audio ? {audioDevice: prefs.audio} : {}),
            ...(prefs.video ? {videoDevice: prefs.video} : {}),
            workerUrl: CALL_WORKER_URL,
        })
        let audio: AudioContext | null = null
        try {
            audio = new AudioContext()
        } catch {
            // No Web Audio: nobody's tile lights up, and the call goes on.
        }
        const a: Active = {
            audio,
            camera,
            candidate: null,
            channel,
            connected: false,
            epoch: null,
            heard: new Map(),
            keyTimer: setInterval(() => {
                void this.refreshKeys()
                // The timer on screen counts seconds; nothing else redraws it.
                this.prune()
                this.notify()
            }, 1000),
            keys: null,
            lastKeys: null,
            levelTimer: setInterval(() => this.levels(), 120),
            listeners: new Map(),
            muted: false,
            mutedTalk: null,
            nudge: false,
            blur: false,
            captioner: null,
            captions: new Map(),
            recorder: null,
            waiting: false,
            watching: '',
            seen: new Set(),
            agenda: null,
            endingForAll: false,
            recorded: 0,
            guestSecret: guest?.secret ?? null,
            dropped: new Map(),
            participant: null,
            probe: null,
            sharing: false,
            since: null,
            speaker: null,
            started: Date.now(),
            tiles,
            toldMuted: null,
            web,
        }
        this.active = a
        if (keys) this.applyKeys(keys)
        if (camera) void web.setCamera(true)
        if (prefs.blur) void web.setBlur(true)
        if (prefs.muted) {
            a.muted = true
            web.setMuted(true)
        }
        void web.start()
        this.notify()
    }

    /** Hear `participant`'s sound for who is talking (never played from here: the page's audio element plays it). */
    private listen(participant: number, stream: MediaStream | null): void {
        const a = this.active
        if (!a?.audio) return
        a.listeners.get(participant)?.source.disconnect()
        a.listeners.delete(participant)
        if (!stream) return
        const source = a.audio.createMediaStreamSource(stream)
        const analyser = a.audio.createAnalyser()
        analyser.fftSize = 512
        source.connect(analyser)
        a.listeners.set(participant, {analyser, source})
    }

    /** Who is talking, who has the big tile, and whether this device talks while muted. */
    private levels(): void {
        const a = this.active
        if (!a?.audio) return
        const now = Date.now()
        const buf = new Float32Array(512)
        let loudest: {level: number; participant: number} | null = null
        for (const [participant, l] of a.listeners) {
            const level = rms(l.analyser, buf)
            if (level > SPEAKING_RMS) {
                a.heard.set(participant, now)
                if (!loudest || level > loudest.level) loudest = {level, participant}
            }
        }
        if (!a.probe) {
            const track = a.web.levelTrack()
            if (track) {
                const source = a.audio.createMediaStreamSource(new MediaStream([track]))
                a.probe = a.audio.createAnalyser()
                a.probe.fftSize = 512
                source.connect(a.probe)
            }
        }
        const mine = a.probe ? rms(a.probe, buf) : 0
        if (mine > SPEAKING_RMS * 2 && !a.muted) a.heard.set(-1, now)
        // The big tile moves to somebody who has talked a while, not at a cough.
        if (loudest) {
            if (a.candidate?.participant !== loudest.participant) a.candidate = {participant: loudest.participant, since: now}
            else if (now - a.candidate.since > SPEAKER_SWITCH_MS && a.speaker !== loudest.participant) {
                a.speaker = loudest.participant
                this.changed()
            }
        }
        // Talking into a muted microphone: say so, once, after a moment.
        const talking = a.muted && mine > SPEAKING_RMS * 2
        if (talking) a.mutedTalk ??= now
        else if (a.mutedTalk !== null && now - a.mutedTalk > 4000) a.mutedTalk = null
        const nudge = a.muted && a.mutedTalk !== null && now - a.mutedTalk > NUDGE_AFTER_MS && !this.self()?.held
        if (nudge !== a.nudge) {
            a.nudge = nudge
            this.changed()
        }
    }

    /** Drop notices and reactions that have had their time. */
    private prune(): void {
        const now = Date.now()
        this.reactions = this.reactions.filter((r) => now - r.at < REACTION_MS)
        this.toasts = this.toasts.filter((t) => t.kind === 'hand' || now - t.at < TOAST_MS)
    }

    private onWeb(channel: string, kind: string, value: string): void {
        const a = this.active
        if (!a || a.channel !== channel) return
        switch (kind) {
            case 'offer':
                this.client.callSignal({channel, offer: value, screen: a.web.screenMid ?? '', type: 'call_join'})
                break
            case 'answer':
                this.client.callSignal({channel, sdp: value, type: 'call_answer'})
                break
            case 'connected':
                a.connected = true
                if (this.others(channel).length && a.since === null) a.since = Date.now()
                break
            case 'failed':
                this.hangUp(value)
                break
            case 'notice':
                this.toast({kind: 'info', text: value})
                break
            case 'camera-off':
                a.camera = false
                break
            case 'screen-off':
                a.sharing = false
                this.control({Sharing: false})
                break
        }
        this.notify()
    }

    /** A commit moves the chat to a new epoch, and the call's key with it. */
    private async refreshKeys(): Promise<void> {
        const a = this.active
        if (!a) return
        // Nobody answered a call placed here. Not while waiting to be let in,
        // which takes as long as the hosts take.
        if (a.since === null && !a.waiting && !this.others(a.channel).length && Date.now() - a.started > NO_ANSWER_MS) {
            this.hangUp('No answer')
            return
        }
        // A guest's key comes from a host (`call_key_dropped`), not the chat.
        if (a.guestSecret) return
        const keys = await this.client.callKeys(a.channel)
        if (keys) this.applyKeys(keys)
    }

    /** Make a guest link to this page's call. The server answers with its
     *  code (`call_invited`); the secret is made here and never sent to it. */
    inviteGuest(): void {
        const a = this.active
        if (!a || a.guestSecret) return
        this.asking.set(a.channel, crypto.getRandomValues(new Uint8Array(16)))
        this.client.callSignal({channel: a.channel, type: 'call_invite'})
    }

    /** Come into a meeting by a guest link, as `name`. Signs this page in as
     *  a guest; the call is joined when the server says which it is. */
    joinAsGuest(link: {code: string; secret: Uint8Array}, name: string, camera: boolean): void {
        this.guest = {camera, channel: null, name, secret: link.secret}
        this.client.signIn({guest: {code: link.code, name}})
        this.notify()
    }

    /** The secret of a guest link to `channel`: made on this page, or read
     *  from the chat it was posted in. */
    private linkSecret(channel: string): Uint8Array | null {
        const known = this.links.get(channel)
        if (known) return known
        const lines = this.client.histories.get(channel) ?? []
        for (let i = lines.length - 1; i >= 0; i--) {
            const link = readGuestLink(lines[i].body)
            if (link) {
                this.links.set(channel, link.secret)
                return link.secret
            }
        }
        return null
    }

    /** Hosting: hand every guest in the call this epoch's key, once each,
     *  sealed under the link's secret. */
    private async dropKeys(): Promise<void> {
        const a = this.active
        if (!a || !a.keys || a.guestSecret || !this.hosting()) return
        const keys = a.keys
        const guests = this.members().filter((m) => m.guest && a.dropped.get(m.participant) !== keys.epoch)
        if (!guests.length) return
        const secret = this.linkSecret(a.channel)
        if (!secret) return
        for (const g of guests) {
            a.dropped.set(g.participant, keys.epoch)
            const sealed = await sealGuestKey(secret, groupBytes(a.channel), {
                baseKey: keys.key,
                epoch: BigInt(keys.epoch),
                members: [],
                ownIndex: guestIndex(g.participant),
            })
            this.client.callSignal({channel: a.channel, participant: g.participant, sealed: hex(sealed), type: 'call_key_drop'})
        }
    }

    private applyKeys(keys: {epoch: number; index: number; key: string}): void {
        const a = this.active
        if (!a || a.epoch === keys.epoch) return
        a.epoch = keys.epoch
        const key = unhex(keys.key)
        a.lastKeys = a.keys
        a.keys = {epoch: keys.epoch, key}
        a.web.keys(keys.epoch, key, keys.index)
        // A new epoch is a new key: every guest in the call needs it too.
        void this.dropKeys()
    }

    /** A guest: the key a host handed this page, opened with the link's secret. */
    private async keyDropped(secret: Uint8Array, sealed: string): Promise<void> {
        const a = this.active
        if (!a) return
        const key = await openGuestKey(secret, groupBytes(a.channel), unhex(sealed))
        if (!key) return
        this.applyKeys({epoch: Number(key.epoch), index: key.ownIndex, key: hex(key.baseKey)})
        this.notify()
    }

    /** Send a `CallAction` (its serde JSON: `{Hand: true}`, `'MuteAll'`, …). */
    control(action: unknown): void {
        const a = this.active
        if (!a) return
        this.client.callSignal({action: JSON.stringify(action), channel: a.channel, type: 'call_control'})
    }

    /** Tell the call whether this microphone is on, once joined and on every change. */
    private tellMuted(): void {
        const a = this.active
        if (!a || a.participant === null || a.toldMuted === a.muted) return
        a.toldMuted = a.muted
        this.control({Muted: a.muted})
    }

    setMuted(muted: boolean): void {
        const a = this.active
        if (!a) return
        if (!muted && this.self()?.held) {
            this.toast({kind: 'held', sub: 'Raise your hand to ask to talk.', text: 'A host muted you'})
            this.notify()
            return
        }
        a.muted = muted
        a.web.setMuted(muted)
        a.mutedTalk = null
        a.nudge = false
        this.tellMuted()
        this.notify()
    }

    async setCamera(on: boolean): Promise<void> {
        if (!this.active) return
        this.active.camera = on
        await this.active.web.setCamera(on)
        this.notify()
    }

    /** Share a screen (the browser asks which) or stop. */
    async setScreen(on: boolean, sharp = true): Promise<void> {
        const a = this.active
        if (!a) return
        if (on && !this.settings().may_share && !this.hosting()) {
            this.toast({kind: 'info', text: 'Only the hosts may share a screen in this call'})
            this.notify()
            return
        }
        const sharing = await a.web.setScreen(on, sharp)
        if (sharing !== a.sharing) {
            a.sharing = sharing
            this.control({Sharing: sharing})
        }
        this.notify()
    }

    /** Who waits to be let into this page's call. */
    waiting(): CallWaiter[] {
        return this.active ? (this.waitingOf.get(this.active.channel) ?? []) : []
    }

    /** Whether this page waits for a host to let it in. */
    waitingToJoin(): boolean {
        return this.active?.waiting ?? false
    }

    /** The hosts' spotlight in this page's call. */
    spotlight(): number | null {
        return this.active ? (this.spotlightOf.get(this.active.channel) ?? null) : null
    }

    /** The open breakout rooms in this page's call. */
    rooms(): CallRooms | null {
        return this.active ? (this.roomsOf.get(this.active.channel) ?? null) : null
    }

    /** This device's breakout room; 0 is the main call. */
    myRoom(): number {
        return this.self()?.room ?? 0
    }

    /** Everybody else in this device's room of the call: whom it sees and hears. */
    roomOthers(): CallMember[] {
        const a = this.active
        if (!a) return []
        const room = this.myRoom()
        return this.others(a.channel).filter((m) => m.room === room)
    }

    /** Somebody new waiting is news to whoever runs the call. */
    private knocks(before: CallWaiter[], now: CallWaiter[]): void {
        if (!this.hosting()) return
        for (const w of now) {
            if (before.some((b) => b.participant === w.participant)) continue
            this.toast({kind: 'knock', participant: w.participant, sub: w.guest ? 'Guest · invite link' : 'Wants to join', text: `${callName(w)} is waiting`})
        }
        // Somebody let in, turned away or gone takes the notice with them.
        this.toasts = this.toasts.filter((t) => t.kind !== 'knock' || now.some((w) => w.participant === t.participant))
    }

    admit(participant: number): void {
        this.control({Admit: {participant}})
    }

    deny(participant: number): void {
        this.control({Deny: {participant}})
    }

    admitAll(): void {
        this.control('AdmitAll')
    }

    setSpotlight(participant: number | null): void {
        this.control({Spotlight: {participant}})
    }

    /** Split into `names.length` rooms, the people not hosting shared out in
     *  turn after a shuffle (a host moves anybody afterwards). */
    openRooms(names: string[], minutes: number, mayMove: boolean): void {
        const people = this.members()
            .filter((m) => m.role === 'member')
            .map((m) => m.participant)
            .sort(() => Math.random() - 0.5)
        const assign = people.map((p, i) => [p, (i % names.length) + 1])
        this.control({OpenRooms: {assign, may_move: mayMove, minutes, names}})
    }

    closeRooms(): void {
        this.control('CloseRooms')
    }

    moveTo(participant: number, room: number): void {
        this.control({MoveTo: {participant, room}})
    }

    /** Whether this page records the call. */
    recording(): boolean {
        return !!this.active?.recorder
    }

    /** Record the call on this device, or stop and save the file. The call is
     *  told first; it refuses unless the hosts allow recording, and everybody
     *  sees who records. */
    async setRecording(on: boolean, title: string): Promise<void> {
        const a = this.active
        if (!a) return
        if (on && !a.recorder) {
            if (!CallRecorder.supported()) {
                this.toast({kind: 'info', text: 'This browser cannot record a call'})
            } else if (!this.settings().may_record) {
                this.toast({kind: 'info', text: 'The hosts have not allowed recording in this call'})
            } else {
                this.control({Recording: true})
                a.recorder = new CallRecorder(() => this.recorded(), title)
            }
        } else if (!on && a.recorder) {
            const r = a.recorder
            a.recorder = null
            this.control({Recording: false})
            await r.stop()
        }
        this.notify()
    }

    /** What a recording draws and mixes, this frame: this room, as on the stage. */
    private recorded() {
        const a = this.active
        if (!a) return []
        const people = this.roomOthers().map((m) => {
            const t = a.tiles.get(m.participant)
            return {audio: t?.audio ?? null, name: callName(m), screen: m.sharing ? (t?.screen ?? null) : null, video: t?.video ?? null}
        })
        const mic = a.web.levelTrack()
        people.push({audio: mic && !a.muted ? new MediaStream([mic]) : null, name: 'You', screen: a.web.screenView(), video: a.web.selfView()})
        return people
    }

    /** Whether this device makes captions of its own voice. */
    captioning(): boolean {
        return !!this.active?.captioner
    }

    /** Make captions of this device's voice, on the device, and send them
     *  sealed; or stop. */
    async setCaptions(on: boolean, lang = this.captionLang): Promise<void> {
        const a = this.active
        if (!a) return
        a.captioner?.stop()
        a.captioner = null
        if (on) {
            this.captionLang = lang
            const started = await Captioner.start(lang, (line) => {
                if (a.muted) return
                this.caption(-1, line.text, line.final)
                void this.sendEvent(JSON.stringify({c: line.text, f: line.final}))
            })
            if ('why' in started) this.toast({kind: 'info', text: started.why})
            else if (this.active === a) a.captioner = started.captioner
        }
        this.notify()
    }

    /** A line of somebody's captions, and the transcript when it is done. */
    private caption(participant: number, text: string, final: boolean): void {
        const a = this.active
        if (!a) return
        a.captions.set(participant, {at: Date.now(), final, text})
        if (final) {
            const who = participant === -1 ? 'You' : (this.members().find((m) => m.participant === participant)?.username ?? 'Somebody')
            this.transcript.push({at: Date.now(), text, who})
        }
        this.notify()
    }

    /** The captions to show: the newest line of whoever spoke last, if recent. */
    captionNow(): {participant: number; text: string; who: string} | null {
        const a = this.active
        if (!a) return null
        let best: {at: number; participant: number; text: string} | null = null
        for (const [participant, c] of a.captions) {
            if (Date.now() - c.at > 5000) continue
            if (!best || c.at > best.at) best = {at: c.at, participant, text: c.text}
        }
        if (!best) return null
        const who = best.participant === -1 ? 'You' : (this.members().find((m) => m.participant === best.participant)?.username ?? '')
        return {participant: best.participant, text: best.text, who}
    }

    /** Whether the camera can blur what is behind the person. */
    blurSupported(): boolean {
        return this.active?.web.blurSupported() ?? false
    }

    async setBlur(on: boolean): Promise<void> {
        const a = this.active
        if (!a) return
        a.blur = await a.web.setBlur(on)
        if (on && !a.blur) this.toast({kind: 'info', text: 'This camera cannot blur its background here'})
        this.notify()
    }

    async setDevices(devices: {audio?: string | null; video?: string | null}): Promise<void> {
        const a = this.active
        if (!a) return
        if (devices.audio !== undefined) await a.web.setAudioDevice(devices.audio)
        if (devices.video !== undefined) await a.web.setVideoDevice(devices.video)
        this.notify()
    }

    /** Tell the forwarder whose cameras this page draws: `null` is everybody's,
     *  `[]` nobody's (the call folded to its bar). Only on a change. */
    watch(cameras: number[] | null): void {
        const a = this.active
        if (!a || a.participant === null) return
        const key = JSON.stringify(cameras)
        if (key === a.watching) return
        a.watching = key
        this.client.callSignal({cameras: key, channel: a.channel, type: 'call_watch'})
    }

    /** Seal and send a call event (a reaction, a line of captions). */
    private async sendEvent(text: string): Promise<void> {
        const a = this.active
        if (!a?.keys) return
        const keys = a.keys
        const sealed = await sealEvent(keys.key, BigInt(keys.epoch), new TextEncoder().encode(text))
        this.client.callSignal({channel: a.channel, epoch: String(keys.epoch), sealed: hex(sealed), type: 'call_react'})
    }

    raiseHand(up: boolean): void {
        this.control({Hand: up})
    }

    /** React with `emoji`, sealed so the server relays it unread. */
    async react(emoji: string): Promise<void> {
        const a = this.active
        if (!a?.keys) return
        const keys = a.keys
        const sealed = await sealEvent(keys.key, BigInt(keys.epoch), new TextEncoder().encode(emoji))
        this.client.callSignal({channel: a.channel, epoch: String(keys.epoch), sealed: hex(sealed), type: 'call_react'})
        this.reactions.push({at: Date.now(), emoji, id: nextId++, participant: -1})
        this.notify()
    }

    private async reacted(participant: number, epoch: string, sealed: string): Promise<void> {
        const a = this.active
        if (!a) return
        const keys = [a.keys, a.lastKeys].find((k) => k && String(k.epoch) === epoch)
        if (!keys) return
        const plain = await openEvent(keys.key, BigInt(epoch), unhex(sealed))
        if (!plain) return
        const text = new TextDecoder().decode(plain)
        // A line of captions or the agenda is JSON; a reaction is the emoji itself.
        if (text.startsWith('{')) {
            try {
                const line = JSON.parse(text) as {c?: string; f?: boolean; g?: number; p?: string; t?: number}
                if (typeof line.c === 'string') this.caption(participant, line.c.slice(0, 300), !!line.f)
                // Only a host keeps the agenda.
                const from = this.members().find((m) => m.participant === participant)
                if (typeof line.g === 'number' && typeof line.p === 'string' && typeof line.t === 'number' && from && from.role !== 'member') {
                    a.agenda = line.g < 0 ? null : {index: line.g, plan: line.p, startedMs: line.t}
                    this.notify()
                }
            } catch {
                // Not ours to read.
            }
            return
        }
        if ([...text].length > 16) return
        this.reactions.push({at: Date.now(), emoji: text, id: nextId++, participant})
        this.notify()
    }

    /** Whether the "you're muted" nudge is up. */
    nudging(): boolean {
        return this.active?.nudge ?? false
    }

    selfView(): MediaStream | null {
        return this.active?.web.selfView() ?? null
    }

    screenView(): MediaStream | null {
        return this.active?.web.screenView() ?? null
    }

    /** Decline the call ringing in `channel`. */
    decline(channel: string): void {
        this.dismissed.add(channel)
        this.client.callSignal({channel, type: 'call_decline'})
        this.notify()
    }

    /** Where the meeting is on its agenda, if a host has started it. */
    agenda(): {index: number; plan: string; startedMs: number} | null {
        return this.active?.agenda ?? null
    }

    /** A host moves the agenda to item `index` of `plan` (or stops it with
     *  `-1`), and everybody is told, sealed. */
    setAgenda(plan: string, index: number): void {
        const a = this.active
        if (!a) return
        a.agenda = index < 0 ? null : {index, plan, startedMs: Date.now()}
        this.sendAgenda(plan)
        this.notify()
    }

    private sendAgenda(plan?: string): void {
        const a = this.active
        if (!a) return
        const g = a.agenda
        void this.sendEvent(JSON.stringify(g ? {g: g.index, p: g.plan, t: g.startedMs} : {g: -1, p: plan ?? '', t: 0}))
    }

    /** A host ends the call for everybody; this device writes the record. */
    endForAll(): void {
        const a = this.active
        if (!a) return
        a.endingForAll = true
        this.control('EndForAll')
    }

    /**
     * Post what the meeting came to into its chat: the notes, decisions and
     * actions taken in it, the polls and how they went, and the transcript if
     * captions were on here. Nothing is posted for a meeting that left none of
     * these, unless `always`.
     */
    postRecord(always = false): boolean {
        const a = this.active
        if (!a || a.since === null) return false
        const messages = this.client.histories.get(a.channel) ?? []
        const title = this.client.chats.find((c) => c.channel === a.channel)?.label ?? ''
        const record = buildRecord(messages, a.since, Date.now(), [...a.seen], this.transcript.filter((l) => l.at >= a.since!), title)
        const size = record.notes.length + record.polls.length + record.transcript.length
        if (!always && size <= a.recorded) return false
        a.recorded = size
        this.client.sendMeeting(a.channel, {kind: 'record', record})
        return true
    }

    hangUp(why?: string): void {
        const a = this.active
        if (!a) return
        this.client.callSignal({channel: a.channel, type: 'call_leave'})
        this.close(why)
    }

    /** End this page's part in the call: everything local, nothing said to the server. */
    private close(why?: string): void {
        const a = this.active
        if (!a) return
        // The record is written once, by the host who ends it for everybody
        // or is the last one in it.
        try {
            const hosting = this.hosting()
            if (a.endingForAll || (hosting && !this.others(a.channel).length)) this.postRecord()
        } catch (e) {
            console.warn('call: the record', e)
        }
        this.active = null
        // Left here: neither "ringing" while the others are still in it, nor
        // "missed" when it ends.
        this.dismissed.add(a.channel)
        this.toasts = []
        this.reactions = []
        if (why) this.notice = why
        clearInterval(a.keyTimer)
        clearInterval(a.levelTimer)
        a.captioner?.stop()
        // A recording in progress is saved, not lost, when the call ends.
        if (a.recorder) void a.recorder.stop()
        // The media last, and nothing it throws keeps the page from saying why.
        try {
            for (const l of a.listeners.values()) l.source.disconnect()
            void a.audio?.close().catch(() => undefined)
            a.web.close()
        } catch (e) {
            console.warn('call: closing', e)
        }
        this.notify()
    }
}
