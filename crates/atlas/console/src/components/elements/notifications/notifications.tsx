import {$s, notifier} from '@/app'

export const Notifications = () => {
    if (!$s.notifications.length) {
        return null
    }
    return (
        <div class="notifications">
            {$s.notifications.map((notification) => (
                <div class={`notification ${notification.level}`} key={notification.id} role="status">
                    <p>{notification.message}</p>
                    <button aria-label="Dismiss" onClick={() => notifier.dismiss(notification.id)} type="button">
                        ✕
                    </button>
                </div>
            ))}
        </div>
    )
}
