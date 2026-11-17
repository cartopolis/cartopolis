/**
 * Waking this browser for a call or a private message while the chat page is
 * closed (`docs/notes/calls.md`, "Ringing a closed browser", and
 * `docs/notes/message-wake-ups.md`).
 *
 * The browser's own push service carries it — Google's for Chrome and Edge,
 * Mozilla's for Firefox, Apple's for Safari — and wakes `sw.js`, which shows
 * "Incoming call" or "New message"; opening it opens this page, which connects
 * and rings or shows the chat as it always does. The push carries one word,
 * encrypted to this browser: which of the two it is. Not who, not which chat.
 * What the push service learns is that this browser got a push from this
 * server at that moment.
 *
 * Only with notifications allowed (the page asks on a click, as it does for
 * messages), and only while signed in: signing out takes the address back and
 * drops the subscription.
 */

/** A browser's address is registered with this in front (`simulator::push::WEB_PREFIX`). */
export const WEB_PREFIX = 'webpush:'

export function webPushSupported(): boolean {
    return (
        typeof navigator !== 'undefined' &&
        'serviceWorker' in navigator &&
        typeof window !== 'undefined' &&
        'PushManager' in window &&
        typeof Notification !== 'undefined'
    )
}

/** The bytes of a base64url string, as `applicationServerKey` takes them. */
export function fromBase64Url(text: string): Uint8Array<ArrayBuffer> {
    const b64 = text.replace(/-/g, '+').replace(/_/g, '/') + '='.repeat((4 - (text.length % 4)) % 4)
    const raw = atob(b64)
    const out = new Uint8Array(raw.length)
    for (let i = 0; i < raw.length; i++) out[i] = raw.charCodeAt(i)
    return out
}

/** `bytes` as base64url without padding, as the server reads a subscription's keys. */
export function toBase64Url(bytes: ArrayBuffer): string {
    let raw = ''
    for (const b of new Uint8Array(bytes)) raw += String.fromCharCode(b)
    return btoa(raw).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '')
}

function sameKey(a: ArrayBuffer | null | undefined, b: Uint8Array): boolean {
    if (!a) return false
    const x = new Uint8Array(a)
    return x.length === b.length && x.every((v, i) => v === b[i])
}

export class WebPush {
    /** What this page registered on the current connection, if anything. */
    private registered: string | null = null
    private send: (endpoint: string) => void

    constructor(send: (endpoint: string) => void) {
        this.send = send
    }

    /**
     * Subscribe (or keep the subscription this browser already has) and tell
     * the server. Called on every connect — the server keeps the newest owner
     * of an address, so a browser shared by two accounts rings the one signed
     * in — and once notifications are allowed.
     */
    async enable(): Promise<void> {
        if (!webPushSupported() || Notification.permission !== 'granted') return
        try {
            const reply = await fetch('/api/push/key')
            // A server that wakes nothing has no key.
            if (!reply.ok) return
            const {key} = (await reply.json()) as {key: string}
            const serverKey = fromBase64Url(key)
            const registration = await navigator.serviceWorker.register('/sw.js')
            let subscription = await registration.pushManager.getSubscription()
            // Subscribed against another key (a server that made a new one):
            // the push service would refuse every push signed with this one.
            if (subscription && !sameKey(subscription.options.applicationServerKey, serverKey)) {
                await subscription.unsubscribe()
                subscription = null
            }
            subscription ??= await registration.pushManager.subscribe({
                applicationServerKey: serverKey,
                userVisibleOnly: true,
            })
            // The subscription's keys after the address say this page tells a
            // message from a call, and let the server encrypt which it is to
            // this browser alone (`simulator::push::seal`).
            const p256dh = subscription.getKey('p256dh')
            const auth = subscription.getKey('auth')
            const keys = p256dh && auth ? `#${toBase64Url(p256dh)}.${toBase64Url(auth)}` : ''
            this.registered = WEB_PREFIX + subscription.endpoint + keys
            this.send(this.registered)
        } catch (e) {
            console.warn('web push not set up:', e)
        }
    }

    /** Signing out: take the address back on this connection, and drop it. */
    async disable(): Promise<void> {
        if (this.registered) this.send('')
        this.registered = null
        if (!webPushSupported()) return
        try {
            const registration = await navigator.serviceWorker.getRegistration('/sw.js')
            const subscription = await registration?.pushManager.getSubscription()
            await subscription?.unsubscribe()
        } catch (e) {
            console.warn('web push not taken down:', e)
        }
    }
}

/**
 * The tag every "Incoming call" notification carries, the page's own and
 * `sw.js`'s alike, so a new one replaces an old one instead of piling up.
 */
export const CALL_TAG = 'cartopolis-call'

/** Take down `sw.js`'s "Incoming call" once the call has stopped ringing. */
export function closeCallNotifications(): void {
    if (!webPushSupported()) return
    void navigator.serviceWorker
        .getRegistration('/sw.js')
        .then((r) => r?.getNotifications({tag: CALL_TAG}))
        .then((list) => list?.forEach((n) => n.close()))
        .catch(() => {})
}
