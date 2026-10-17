/**
 * Recording a call on this device (`docs/notes/calls.md`, "A call is a
 * meeting"). The server cannot record — it never holds a key — so a recording
 * only ever exists where it is made: here, from what this page already
 * receives. Every picture on the stage is drawn into one canvas at 15 frames
 * a second, everybody's sound (this device's microphone too) is mixed into
 * one track, and a `MediaRecorder` writes WebM that the person saves when it
 * stops. The call is told (`CallAction::Recording`) before the first frame,
 * and refuses it unless the hosts allow recording, so nobody is recorded
 * without being shown that they are.
 */

/** One person's picture and sound, as the page holds them. */
export interface RecordedSource {
    audio: MediaStream | null
    name: string
    /** A shared screen is drawn large, before the cameras. */
    screen: MediaStream | null
    video: MediaStream | null
}

const FPS = 15
const WIDTH = 1280
const HEIGHT = 720

/** Pick a type this browser records, VP9 first. */
function mimeType(): string {
    for (const t of ['video/webm;codecs=vp9,opus', 'video/webm;codecs=vp8,opus', 'video/webm']) {
        if (typeof MediaRecorder !== 'undefined' && MediaRecorder.isTypeSupported(t)) return t
    }
    return ''
}

export class CallRecorder {
    private canvas = Object.assign(document.createElement('canvas'), {height: HEIGHT, width: WIDTH})
    private g = this.canvas.getContext('2d')!
    private audio = new AudioContext()
    private mix = this.audio.createMediaStreamDestination()
    private heard = new Map<MediaStream, MediaStreamAudioSourceNode>()
    private videos = new Map<MediaStream, HTMLVideoElement>()
    private chunks: Blob[] = []
    private recorder: MediaRecorder
    private timer: ReturnType<typeof setInterval>
    readonly started = Date.now()

    static supported(): boolean {
        return typeof MediaRecorder !== 'undefined' && mimeType() !== '' && 'captureStream' in HTMLCanvasElement.prototype
    }

    /** Start recording what `sources` returns, asked again every frame. */
    constructor(private sources: () => RecordedSource[], private title: string) {
        const stream = new MediaStream([...this.canvas.captureStream(FPS).getVideoTracks(), ...this.mix.stream.getAudioTracks()])
        this.recorder = new MediaRecorder(stream, {mimeType: mimeType(), videoBitsPerSecond: 2_500_000})
        this.recorder.ondataavailable = (e) => {
            if (e.data.size) this.chunks.push(e.data)
        }
        this.recorder.start(1000)
        this.timer = setInterval(() => this.frame(), 1000 / FPS)
    }

    private videoOf(stream: MediaStream): HTMLVideoElement {
        let v = this.videos.get(stream)
        if (!v) {
            v = Object.assign(document.createElement('video'), {autoplay: true, muted: true, playsInline: true, srcObject: stream})
            void v.play().catch(() => undefined)
            this.videos.set(stream, v)
        }
        return v
    }

    /** One frame: the screen large if anybody shares, then everybody's camera
     *  in an even grid with their name. Sound sources are kept in step. */
    private frame(): void {
        const sources = this.sources()
        const g = this.g
        g.fillStyle = '#0b0f10'
        g.fillRect(0, 0, WIDTH, HEIGHT)
        const screen = sources.find((s) => s.screen)
        let area = {h: HEIGHT, w: WIDTH, x: 0, y: 0}
        if (screen?.screen) {
            const v = this.videoOf(screen.screen)
            const w = WIDTH * 0.78
            this.contain(v, 0, 0, w, HEIGHT)
            area = {h: HEIGHT, w: WIDTH - w, x: w, y: 0}
        }
        const n = Math.max(sources.length, 1)
        const cols = area.w < WIDTH ? 1 : Math.ceil(Math.sqrt(n))
        const rows = Math.ceil(n / cols)
        const cw = area.w / cols
        const ch = area.w < WIDTH ? Math.min(area.h / rows, cw * 0.5625) : area.h / rows
        sources.forEach((s, i) => {
            const x = area.x + (i % cols) * cw
            const y = area.y + Math.floor(i / cols) * ch
            g.fillStyle = '#1a2224'
            g.fillRect(x + 3, y + 3, cw - 6, ch - 6)
            if (s.video) this.cover(this.videoOf(s.video), x + 3, y + 3, cw - 6, ch - 6)
            g.fillStyle = 'rgb(0 0 0 / 55%)'
            g.font = '600 18px system-ui, sans-serif'
            const tw = g.measureText(s.name).width
            g.fillRect(x + 10, y + ch - 36, tw + 16, 26)
            g.fillStyle = '#fff'
            g.fillText(s.name, x + 18, y + ch - 17)
        })
        // Sound: everybody heard, nobody twice, and those gone let go.
        const live = new Set(sources.map((s) => s.audio).filter((a): a is MediaStream => !!a))
        for (const a of live) {
            if (this.heard.has(a) || !a.getAudioTracks().length) continue
            const node = this.audio.createMediaStreamSource(a)
            node.connect(this.mix)
            this.heard.set(a, node)
        }
        for (const [a, node] of this.heard) {
            if (!live.has(a)) {
                node.disconnect()
                this.heard.delete(a)
            }
        }
    }

    private contain(v: HTMLVideoElement, x: number, y: number, w: number, h: number): void {
        if (!v.videoWidth) return
        const s = Math.min(w / v.videoWidth, h / v.videoHeight)
        const dw = v.videoWidth * s
        const dh = v.videoHeight * s
        this.g.drawImage(v, x + (w - dw) / 2, y + (h - dh) / 2, dw, dh)
    }

    private cover(v: HTMLVideoElement, x: number, y: number, w: number, h: number): void {
        if (!v.videoWidth) return
        const s = Math.max(w / v.videoWidth, h / v.videoHeight)
        const sw = w / s
        const sh = h / s
        this.g.drawImage(v, (v.videoWidth - sw) / 2, (v.videoHeight - sh) / 2, sw, sh, x, y, w, h)
    }

    /** Stop, and hand the person the file: it never leaves this device unless they send it. */
    async stop(): Promise<void> {
        clearInterval(this.timer)
        const done = new Promise<void>((resolve) => (this.recorder.onstop = () => resolve()))
        if (this.recorder.state !== 'inactive') this.recorder.stop()
        await done
        for (const node of this.heard.values()) node.disconnect()
        for (const v of this.videos.values()) v.srcObject = null
        void this.audio.close()
        const blob = new Blob(this.chunks, {type: this.recorder.mimeType || 'video/webm'})
        const stamp = new Date(this.started).toISOString().slice(0, 16).replace('T', ' ').replace(':', 'h')
        const a = Object.assign(document.createElement('a'), {
            download: `${this.title} ${stamp}.webm`,
            href: URL.createObjectURL(blob),
        })
        a.click()
        setTimeout(() => URL.revokeObjectURL(a.href), 60_000)
    }
}
