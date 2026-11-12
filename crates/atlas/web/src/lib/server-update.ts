import {api} from '@/app'
import {ApiError} from '@/lib/api'

/**
 * A self-hosted server's own updates (`crates/server/src/update.rs`, served at
 * `/api/admin/update` by `crates/simulator/src/server_update.rs`): what is
 * installed, a release signed by the owner's key that is ready, and how an
 * install went. A server that does not update itself (cartopolis.org) answers
 * 404, and the page shows nothing about updates.
 */
export interface ServerUpdate {
    available: {
        changes: string[]
        code: number
        name: string
        published: string
    } | null
    installed: {code: number; name: string}
    last_check: number | null
    last_result: {
        at: number
        back_to?: number
        by: string
        code: number
        from?: number
        result: 'installed' | 'rolled back'
    } | null
    message: string | null
    phase: 'checking' | 'downloading' | 'failed' | 'idle' | 'installing' | 'ready' | 'restarting'
    previous: {code: number; name: string} | null
    signed_by: string | null
    source: string
    steps: string[]
    trusted_keys: string[]
}

/** The server's update state, or null where it has none or this account may not see it. */
export async function loadServerUpdate(): Promise<ServerUpdate | null> {
    try {
        return await api.get<ServerUpdate>('/api/admin/update')
    } catch {
        return null
    }
}

/**
 * Install the ready release: the server asks the password again, then tests,
 * switches and restarts. `'reauth'` for an account without a password that
 * must sign in through its provider again first.
 */
export async function installServerUpdate(password: string | undefined): Promise<ServerUpdate | 'reauth' | string> {
    try {
        return await api.post<ServerUpdate>('/api/admin/update/install', {password})
    } catch (e) {
        if (e instanceof ApiError) return e.reauth ? 'reauth' : e.message
        return 'Could not reach the server'
    }
}
