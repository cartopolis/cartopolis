import m from '@garage44/mithril'

/**
 * The World tab's frame and the page around it (`docs/notes/web-client-is-the-world.md`).
 *
 * The app's web build in `/viewer/` is only the world: it has no sign-in, no
 * chats and no calls of its own (its `comms` feature is off), because this page
 * has them. So the page does two things for it:
 *
 * - **It signs the frame in.** The frame says `{type: 'cartopolis.ready'}` once
 *   it listens, and the page answers with `{type: 'cartopolis.session', token}`
 *   (`framed_session.rs`) — and posts it again whenever it signs in or out. The
 *   token is the one the chat worker says hello with, so the world is the same
 *   account and is never a second device.
 * - **It loads the frame only where it can run.** The web build needs WebGPU
 *   (`crates/cartopolis/Cargo.toml` says why WebGL2 is no fallback), so World
 *   asks the browser for an adapter first, and without one says so instead of
 *   downloading a module that cannot start. Community works either way.
 *
 * Both messages stay on this origin: the page posts only to `location.origin`
 * and reads only messages from it, as `place_link::follow_framed_places` does.
 */

/** Whether this browser can run the world: unknown until the adapter answers. */
export type WorldGpu = 'unknown' | 'yes' | 'no'

let gpu: WorldGpu = 'unknown'
let asked = false

/** What the browser said about WebGPU; the first call asks, and redraws on the answer. */
export function worldGpu(): WorldGpu {
    if (!asked) {
        asked = true
        void hasAdapter().then((ok) => {
            gpu = ok ? 'yes' : 'no'
            m.redraw()
        })
    }
    return gpu
}

async function hasAdapter(): Promise<boolean> {
    const nav = navigator as Navigator & {gpu?: {requestAdapter(): Promise<unknown>}}
    if (!nav.gpu) return false
    try {
        return (await nav.gpu.requestAdapter()) !== null
    } catch {
        return false
    }
}

/** The page's side of the frame: the frame to post to, and the token it was last given. */
export class WorldSession {
    frame: HTMLIFrameElement | null = null
    /** `undefined` until the frame has been told anything at all. */
    sent: string | null | undefined = undefined
    private listening = false

    constructor(private token: () => string | null) {}

    /** Bind to the frame, and answer its `ready` from now on. */
    attach(frame: HTMLIFrameElement) {
        this.frame = frame
        if (this.listening) return
        this.listening = true
        window.addEventListener('message', (e: MessageEvent) => {
            if (e.origin !== location.origin || e.source !== this.frame?.contentWindow) return
            if ((e.data as {type?: unknown} | null)?.type !== 'cartopolis.ready') return
            // A frame that (re)loaded has forgotten what it was told.
            this.sent = undefined
            this.sync()
        })
    }

    /** Tell the frame the page's session, if it changed since it was last told. */
    sync() {
        const token = this.token() || null
        if (!this.frame?.contentWindow || token === this.sent) return
        this.sent = token
        this.frame.contentWindow.postMessage({token, type: 'cartopolis.session'}, location.origin)
    }
}
