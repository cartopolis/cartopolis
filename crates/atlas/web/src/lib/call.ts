/**
 * Calls on the chat page (`docs/notes/calls.md`, "Calls in the browser").
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
 */
import {WebCall, webCallSupported} from '../../../../cartopolis/web/webcall.js'

import type {ChatClient} from './chat'
import {CallTones} from './call-tones'
import {CALL_TAG, closeCallNotifications} from './web-push'

/** Somebody in a chat's call. `username` is the bare name, as the server keeps it. */
export interface CallMember {
    device: string
    participant: number
    username: string
}

export interface Tile {
    audio: MediaStream | null
    participant: number
    video: MediaStream | null
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
    camera: boolean
    muted: boolean
    participant: number | null
    tiles: Map<number, Tile>
    keyTimer: ReturnType<typeof setInterval>
}

/** How long a call placed here rings before giving up, ms. */
const NO_ANSWER_MS = 60_000

export class Calls {
    active: Active | null = null
    /** Every chat's call, by channel. */
    readonly rosters = new Map<string, CallMember[]>()
    /** Calls this person waved away, so they stop ringing. */
    readonly dismissed = new Set<string>()
    /** Something to tell the person about a call. */
    notice: string | null = null

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

    /** A chat whose call is ringing here, if any. */
    ringing(): string | null {
        for (const [channel, members] of this.rosters) {
            if (!members.length || this.dismissed.has(channel) || this.active?.channel === channel) continue
            if (!members.some((m) => m.username.toLowerCase() === this.me())) return channel
        }
        return null
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
                if (!members.length) {
                    // Over. A call that rang here and nobody here touched is missed.
                    if (before.length && !this.dismissed.has(channel) && a?.channel !== channel) {
                        const who = before[0]?.username
                        this.notice = who ? `Missed call from ${who}` : 'Missed call'
                    }
                    this.dismissed.delete(channel)
                }
                if (a && a.channel === channel) {
                    a.web.forgetGone()
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
                }
                break
            case 'call_offer':
                if (a && a.channel === event.channel) {
                    void a.web.offered(event.sdp as string, event.tracks as {mid: string; participant: number; video: boolean}[])
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
                this.notice = `The call could not start: ${event.message}`
                this.hangUp()
                break
            default:
                return
        }
        this.notify()
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
        const tiles = new Map<number, Tile>()
        const web = new WebCall(
            (kind: string, value: string) => this.onWeb(channel, kind, value),
            {
                onStream: (participant: number, kind: 'audio' | 'video', stream: MediaStream | null) => {
                    const t = tiles.get(participant) ?? {audio: null, participant, video: null}
                    t[kind] = stream
                    if (t.audio || t.video) tiles.set(participant, t)
                    else tiles.delete(participant)
                    this.notify()
                },
                workerUrl: '/call-worker.js',
            },
        )
        const a: Active = {
            camera,
            channel,
            connected: false,
            epoch: null,
            keyTimer: setInterval(() => {
                void this.refreshKeys()
                // The timer on screen counts seconds; nothing else redraws it.
                this.notify()
            }, 1000),
            muted: false,
            participant: null,
            since: null,
            started: Date.now(),
            tiles,
            web,
        }
        this.active = a
        this.applyKeys(keys)
        if (camera) void web.setCamera(true)
        void web.start()
        this.notify()
    }

    private onWeb(channel: string, kind: string, value: string): void {
        const a = this.active
        if (!a || a.channel !== channel) return
        switch (kind) {
            case 'offer':
                this.client.callSignal({channel, offer: value, type: 'call_join'})
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
                this.notice = value
                break
            case 'camera-off':
                a.camera = false
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
        const key = new Uint8Array(keys.key.match(/../g)!.map((h) => parseInt(h, 16)))
        a.web.keys(keys.epoch, key, keys.index)
    }

    setMuted(muted: boolean): void {
        if (!this.active) return
        this.active.muted = muted
        this.active.web.setMuted(muted)
        this.notify()
    }

    async setCamera(on: boolean): Promise<void> {
        if (!this.active) return
        this.active.camera = on
        await this.active.web.setCamera(on)
        this.notify()
    }

    selfView(): MediaStream | null {
        return this.active?.web.selfView() ?? null
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
        this.active = null
        // Left here: neither "ringing" while the others are still in it, nor
        // "missed" when it ends.
        this.dismissed.add(a.channel)
        clearInterval(a.keyTimer)
        a.web.close()
        this.client.callSignal({channel: a.channel, type: 'call_leave'})
        if (why) this.notice = why
        this.notify()
    }
}
