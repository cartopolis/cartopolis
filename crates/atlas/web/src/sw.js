/*
 * The chat page's service worker: it exists to wake this browser for a call or
 * a private message while no chat page is open (`src/lib/web-push.ts`,
 * `docs/notes/calls.md`, `docs/notes/message-wake-ups.md`).
 *
 * The push it is woken by says only which of the two it is, encrypted to this
 * browser — the server says nothing about who is calling or writing, or in
 * which chat, so neither does the push service that carries it. An empty push
 * is a call (a server older than message wake-ups). It shows "Incoming call"
 * or "New message"; opening that opens the chat, which connects and rings with
 * the caller's name, or shows the messages. A chat page already on screen
 * does both by itself, so then this shows nothing.
 *
 * Plain JavaScript, copied to the site root as it is: a service worker's scope
 * is the directory it is served from, and this one has to cover `/`.
 */

self.addEventListener('install', () => self.skipWaiting())
self.addEventListener('activate', (event) => event.waitUntil(self.clients.claim()))

/** What the push woke us for: `'message'`, or a call. */
function wokenFor(event) {
    try {
        return event.data?.text() === 'message' ? 'message' : 'call'
    } catch {
        return 'call'
    }
}

self.addEventListener('push', (event) => {
    const kind = wokenFor(event)
    event.waitUntil(
        self.clients.matchAll({includeUncontrolled: true, type: 'window'}).then((windows) => {
            // Chrome lets a push show nothing only while a page of this site is on
            // screen; that page rings by itself. In a hidden tab both this and the
            // page's own notification appear, under one tag, so the second replaces
            // the first rather than standing beside it.
            if (windows.some((w) => w.visibilityState === 'visible')) return undefined
            if (kind === 'message') {
                // One notice for however many lines: a later one replaces it
                // quietly rather than buzzing again.
                return self.registration.showNotification('New message', {
                    body: 'Somebody wrote to you on Cartopolis. Open it to read.',
                    icon: '/icon.svg',
                    renotify: false,
                    tag: 'cartopolis-message',
                })
            }
            return self.registration.showNotification('Incoming call', {
                body: 'Somebody is calling you on Cartopolis. Open it to answer.',
                icon: '/icon.svg',
                renotify: true,
                requireInteraction: true,
                tag: 'cartopolis-call',
            })
        }),
    )
})

self.addEventListener('notificationclick', (event) => {
    event.notification.close()
    event.waitUntil(
        self.clients.matchAll({includeUncontrolled: true, type: 'window'}).then((windows) => {
            const chat = windows.find((w) => ['/', '/chat'].includes(new URL(w.url).pathname))
            if (chat) return chat.focus()
            return self.clients.openWindow('/chat')
        }),
    )
})
