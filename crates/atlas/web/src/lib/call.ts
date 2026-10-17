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
import {openEvent, sealEvent} from '../../../../cartopolis/web/call-events.js'
import {WebCall, webCallSupported} from '../../../../cartopolis/web/webcall.js'

import type {ChatClient} from './chat'
import {CallTones} from './call-tones'
import {CALL_TAG, closeCallNotifications} from './web-push'

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
}

/** What a call's hosts allow everybody else. */
export interface CallSettings {
    locked: boolean
    may_share: boolean
    may_unmute: boolean
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
    kind: 'hand' | 'held' | 'info' | 'free'
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
    /** Calls this person waved away, so they stop ringing. */
    readonly dismissed = new Set<string>()
    /** Something to tell the person about a call. */
    notice: string | null = null
    /** Notices inside the call, newest last. */
    toasts: Toast[] = []
    /** Reactions in the call, newest last. */
    reactions: Reaction[] = []

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
        return (this.active && this.settingsOf.get(this.active.channel)) || {locked: false, may_share: true, may_unmute: true}
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
                    this.meetingChanges(before, members)
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
                    void a.web.joined(event.answer as string)
                    this.tellMuted()
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
            // A hand going up is news to whoever runs the call.
            if (hosting && m.hand !== null && (old?.hand ?? null) === null) {
                this.toast({kind: 'hand', participant: m.participant, sub: 'In the queue', text: `${m.username} raised a hand`})
            }
        }
        // A hand that went down takes its notice with it.
        this.toasts = this.toasts.filter((t) => t.kind !== 'hand' || now.some((m) => m.participant === t.participant && m.hand !== null))
    }

    /** Place or join the call in `channel`. */
    async join(channel: string, camera = false): Promise<void> {
        if (!Calls.supported()) {
            this.notice = 'This browser cannot make an encrypted call. Try a recent Chrome, Edge, Firefox or Safari.'
            this.notify()
            return
        }
        const keys = await this.client.callKeys(channel)
        if (!keys) {
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
        this.applyKeys(keys)
        if (camera) void web.setCamera(true)
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
        // Nobody answered a call placed here.
        if (a.since === null && !this.others(a.channel).length && Date.now() - a.started > NO_ANSWER_MS) {
            this.hangUp('No answer')
            return
        }
        const keys = await this.client.callKeys(a.channel)
        if (keys) this.applyKeys(keys)
    }

    private applyKeys(keys: {epoch: number; index: number; key: string}): void {
        const a = this.active
        if (!a || a.epoch === keys.epoch) return
        a.epoch = keys.epoch
        const key = unhex(keys.key)
        a.lastKeys = a.keys
        a.keys = {epoch: keys.epoch, key}
        a.web.keys(keys.epoch, key, keys.index)
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
        const emoji = new TextDecoder().decode(plain)
        if ([...emoji].length > 16) return
        this.reactions.push({at: Date.now(), emoji, id: nextId++, participant})
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
        this.active = null
        // Left here: neither "ringing" while the others are still in it, nor
        // "missed" when it ends.
        this.dismissed.add(a.channel)
        this.toasts = []
        this.reactions = []
        if (why) this.notice = why
        clearInterval(a.keyTimer)
        clearInterval(a.levelTimer)
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
