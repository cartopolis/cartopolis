/**
 * The sounds of a call on the chat page, the same ones the app makes
 * (`crates/cartopolis/src/systems/net/call_tones.rs`, and `docs/notes/calls.md`):
 * a ringtone for a call coming in, the 425 Hz ringback while yours rings on
 * the other side, a double beep for a call waiting while you are in another,
 * and short cues when somebody joins or leaves and when the call ends.
 *
 * Synthesised here, as the app synthesises them, rather than shipped as files:
 * the notes, lengths and levels are the app's, so a call sounds the same in
 * both. What plays is decided from the call's state each time it changes, not
 * from events — one looping tone at a time, chosen by `wantedLoop`, and a cue
 * on each change in how many others are in the call (`cueFor`).
 *
 * A browser keeps an `AudioContext` made without a click or a key press
 * suspended, so the context is made at the first gesture on the page and
 * resumed there. Signing in is one, so in practice the page can ring; a call
 * arriving in a tab nobody has touched since it loaded rings silently, and the
 * dialog is still there.
 */

export type Loop = 'ringtone' | 'waiting' | 'ringback'
export type Cue = 'joined' | 'left' | 'ended'

/** The phone rules for which tone loops (`call_tones::wanted_loop`). */
export function wantedLoop(ringing: boolean, inCall: boolean, anyoneElse: boolean): Loop | null {
    if (ringing && !inCall) return 'ringtone'
    if (ringing && inCall && anyoneElse) return 'waiting'
    if (inCall && !anyoneElse) return 'ringback'
    return null
}

/**
 * The cue for a change in how many others are in this device's call; `null`
 * means not in a call.
 */
export function cueFor(before: number | null, after: number | null): Cue | null {
    if (before !== null && after !== null && after > before) return 'joined'
    if (before !== null && after !== null && after < before && after > 0) return 'left'
    if (before !== null && after === null) return 'ended'
    return null
}

export const SAMPLE_RATE = 22_050

const at = (secs: number) => Math.floor(secs * SAMPLE_RATE)

/** A soft bell: a fast attack, an exponential decay, a little octave on top. */
function chime(out: Float32Array, start: number, len: number, freq: number, level: number) {
    const from = at(start)
    const n = at(len)
    const attack = Math.max(at(0.005), 1)
    for (let i = 0; i < n && from + i < out.length; i++) {
        const t = i / SAMPLE_RATE
        const env = i < attack ? i / attack : Math.exp((-5.5 * t) / len)
        out[from + i] += level * env * (Math.sin(t * freq * 2 * Math.PI) + 0.3 * Math.sin(t * freq * 4 * Math.PI))
    }
}

/** A plain tone with 10 ms fades, the way a telephone exchange beeps. */
function beep(out: Float32Array, start: number, len: number, freq: number, level: number) {
    const from = at(start)
    const n = at(len)
    const fade = Math.max(at(0.01), 1)
    for (let i = 0; i < n && from + i < out.length; i++) {
        const t = i / SAMPLE_RATE
        const env = Math.min(Math.min(i, n - i) / fade, 1)
        out[from + i] += level * env * Math.sin(t * freq * 2 * Math.PI)
    }
}

/** Every clip, as samples at `SAMPLE_RATE`. Loops carry their own silence. */
export const clips: Record<Loop | Cue, () => Float32Array<ArrayBuffer>> = {
    ended() {
        const out = new Float32Array(at(0.8))
        ;[659.3, 523.3, 392.0].forEach((f, k) => beep(out, k * 0.22, 0.16, f, 0.22))
        return out
    },
    joined() {
        const out = new Float32Array(at(0.5))
        chime(out, 0, 0.25, 523.3, 0.25)
        chime(out, 0.12, 0.35, 784.0, 0.25)
        return out
    },
    left() {
        const out = new Float32Array(at(0.5))
        chime(out, 0, 0.25, 784.0, 0.22)
        chime(out, 0.12, 0.35, 523.3, 0.22)
        return out
    },
    ringback() {
        const out = new Float32Array(at(5.0))
        beep(out, 0, 1.0, 425.0, 0.28)
        return out
    },
    ringtone() {
        const out = new Float32Array(at(3.0))
        for (const start of [0, 0.62]) {
            ;[659.3, 830.6, 987.8].forEach((f, k) => chime(out, start + k * 0.14, 0.45, f, 0.22))
        }
        return out
    },
    waiting() {
        const out = new Float32Array(at(3.0))
        beep(out, 0, 0.15, 440, 0.2)
        beep(out, 0.3, 0.15, 440, 0.2)
        return out
    },
}

/** The app's levels: the ringtone is meant to be heard across a room. */
const RING_VOLUME = 0.9
const CUE_VOLUME = 0.6

export class CallTones {
    private ctx: AudioContext | null = null
    private buffers = new Map<Loop | Cue, AudioBuffer>()
    private playing: {loop: Loop; node: AudioBufferSourceNode} | null = null
    private others: number | null = null
    /** What the call wants, kept so a context made later can catch up. */
    private want: Loop | null = null

    constructor() {
        if (typeof window === 'undefined' || !('AudioContext' in window)) return
        const unlock = () => {
            if (!this.ctx) this.ctx = new AudioContext()
            void this.ctx.resume().then(() => this.apply())
        }
        for (const ev of ['pointerdown', 'keydown', 'touchend']) {
            addEventListener(ev, unlock, {capture: true, passive: true})
        }
    }

    /** The call's state now: which loop plays, and whether a cue sounds. */
    update(ringing: boolean, inCall: boolean, others: number) {
        this.want = wantedLoop(ringing, inCall, others > 0)
        const now = inCall ? others : null
        const cue = cueFor(this.others, now)
        this.others = now
        this.apply()
        if (cue) this.play(cue, CUE_VOLUME, false)
    }

    private apply() {
        if (this.playing?.loop === this.want) return
        if (this.playing) {
            this.playing.node.stop()
            this.playing = null
        }
        if (!this.want) return
        const node = this.play(this.want, this.want === 'ringtone' ? RING_VOLUME : CUE_VOLUME, true)
        if (node) this.playing = {loop: this.want, node}
    }

    private play(clip: Loop | Cue, volume: number, loop: boolean): AudioBufferSourceNode | null {
        const ctx = this.ctx
        // A node started on a context still suspended plays once it resumes.
        if (!ctx) return null
        let buffer = this.buffers.get(clip)
        if (!buffer) {
            const samples = clips[clip]()
            buffer = ctx.createBuffer(1, samples.length, SAMPLE_RATE)
            buffer.copyToChannel(samples, 0)
            this.buffers.set(clip, buffer)
        }
        const node = ctx.createBufferSource()
        node.buffer = buffer
        node.loop = loop
        const gain = ctx.createGain()
        gain.gain.value = volume
        node.connect(gain).connect(ctx.destination)
        node.start()
        return node
    }
}
