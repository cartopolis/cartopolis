import m, {MithrilComponent} from '@garage44/mithril'
import {Alert} from '@garage44/mithril/components'

import {$s, notifier} from '@/app'

/** The notifier's levels in the library's tone names. */
const TONE = {error: 'danger', info: 'info', success: 'success'} as const

/**
 * The notifier's list, drawn as the library's toasts.
 *
 * Not the library's `Toaster`: that owns its own list and timeouts, and this one
 * is `lib/notifier.ts`'s, where errors deliberately never expire. Same markup and
 * styles, different keeper.
 */
export class Notifications extends MithrilComponent {
    view() {
        return (
            <div aria-live="polite" class="m-toaster">
                {$s.notifications.map((notification) => (
                    <div class="m-toast" key={notification.id}>
                        <Alert ondismiss={() => notifier.dismiss(notification.id)} tone={TONE[notification.level]}>
                            {notification.message}
                        </Alert>
                    </div>
                ))}
            </div>
        )
    }
}
