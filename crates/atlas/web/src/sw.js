/*
 * The chat page's service worker: it exists to ring this browser for a call
 * while no chat page is open (`src/lib/web-push.ts`, `docs/notes/calls.md`).
 *
 * The push it is woken by is empty — the server says nothing about who is
 * calling or in which chat, so neither does the push service that carries it.
 * It shows "Incoming call"; opening that opens the chat, which connects and
 * rings with the caller's name. A chat page already on screen rings by itself,
 * so then this shows nothing.
 *
 * Plain JavaScript, copied to the site root as it is: a service worker's scope
 * is the directory it is served from, and this one has to cover `/`.
 */

self.addEventListener('install', () => self.skipWaiting())
self.addEventListener('activate', (event) => event.waitUntil(self.clients.claim()))

self.addEventListener('push', (event) => {
    event.waitUntil(
        self.clients.matchAll({includeUncontrolled: true, type: 'window'}).then((windows) => {
            // Chrome lets a push show nothing only while a page of this site is on
            // screen; that page rings by itself. In a hidden tab both this and the
            // page's own notification appear, under one tag, so the second replaces
            // the first rather than standing beside it.
            if (windows.some((w) => w.visibilityState === 'visible')) return undefined
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
            const chat = windows.find((w) => new URL(w.url).pathname === '/')
            if (chat) return chat.focus()
            return self.clients.openWindow('/')
        }),
    )
})
