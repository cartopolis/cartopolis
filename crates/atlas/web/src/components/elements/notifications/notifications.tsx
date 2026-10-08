import m, {MithrilComponent} from '@garage44/mithril'

import {$s, notifier} from '@/app'

export class Notifications extends MithrilComponent {
    view() {
        if (!$s.notifications.length) {
            return null
        }
        return (
            <div class="notifications">
                {$s.notifications.map((notification) => (
                    <div class={`notification ${notification.level}`} key={notification.id} role="status">
                        <p>{notification.message}</p>
                        <button aria-label="Dismiss" onclick={() => notifier.dismiss(notification.id)} type="button">
                            ✕
                        </button>
                    </div>
                ))}
            </div>
        )
    }
}
