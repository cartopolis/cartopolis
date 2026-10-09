import {signal, type Children} from '@garage44/mithril'
import {storedTheme, type Theme} from '@garage44/mithril/components'

/**
 * The frame's own state: which panels are open, how wide they are, and what the
 * context panel holds.
 *
 * Signals at module scope rather than fields of `$s`: nothing here should
 * survive a reload or be seen by the Store. An open drawer after a refresh is a
 * drawer nobody asked for.
 */
export const layout = {
    navOpen: signal(false),
    navWidth: signal(232),
    contextOpen: signal(false),
    contextWidth: signal(420),
    theme: signal<Theme>(storedTheme()),
}

/**
 * What the context panel shows. Functions, not vnodes, so the panel re-renders
 * from the page's live state on every redraw instead of freezing what was true
 * when it opened.
 */
export interface ContextContent {
    /** Which thing it is about, so a page can tell its own panel apart. */
    key: string
    title: () => Children
    body: () => Children
    footer?: () => Children
}

const content = signal<ContextContent | null>(null)

/**
 * Put a form or a record's details in the right-hand panel and open it. Pages
 * call this from a row's "Manage", so the list stays in view beside what is
 * being edited on a wide screen, and on a phone the panel covers it.
 */
export function openContext(next: ContextContent): void {
    content.value = next
    layout.contextOpen.value = true
}

/** Close the panel; with a key, only if it still shows that thing. */
export function closeContext(key?: string): void {
    if (key !== undefined && content.value?.key !== key) {
        return
    }
    layout.contextOpen.value = false
}

/** What the panel shows now, or null. Reading it subscribes the view. */
export function contextContent(): ContextContent | null {
    return content.value
}

/** Whether the panel is open on `key`: a row reads it to show itself as selected. */
export function contextShows(key: string): boolean {
    return layout.contextOpen.value && content.value?.key === key
}
