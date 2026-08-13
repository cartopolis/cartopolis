import type {Notification} from '@/types'

/**
 * Transient messages, held in the same store as everything else so the toast
 * list is just another thing a component reads.
 *
 * Errors do not expire. Every error here is the answer to something the user
 * pressed — a refused delete, a taken username — and a message that vanishes
 * while they are reading it is a message they will make the same mistake to see
 * again.
 */
const TIMEOUT_MS = 4000

export class Notifier {
    private list: Notification[] = []

    private next = 1

    init(list: Notification[]) {
        this.list = list
    }

    info(message: string) {
        this.push('info', message)
    }

    success(message: string) {
        this.push('success', message)
    }

    error(message: string) {
        this.push('error', message)
    }

    dismiss(id: number) {
        const index = this.list.findIndex((n) => n.id === id)
        if (index !== -1) {
            this.list.splice(index, 1)
        }
    }

    private push(level: Notification['level'], message: string) {
        const id = this.next++
        this.list.push({id, level, message})
        if (level !== 'error') {
            setTimeout(() => this.dismiss(id), TIMEOUT_MS)
        }
    }
}
