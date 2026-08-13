/**
 * One account, exactly as `auth::AccountRow` serialises it.
 *
 * There is no `pw_hash` here because there is none there — the server type omits
 * it deliberately (see its doc comment), so this interface is a description of
 * the wire rather than a filter applied to it.
 */
export interface Account {
    admin: boolean
    created_at: string
    disabled: boolean
    id: string
    username: string
}

export interface Profile {
    /** The signed-in admin's principal, or `''` when signed out. */
    principal: string
    token: string
    username: string
}

export interface Notification {
    id: number
    /** `error` survives until dismissed; the rest time out. */
    level: 'info' | 'success' | 'error'
    message: string
}

export interface AdminState extends Record<string, unknown> {
    accounts: Account[]
    /** Which account's row is expanded into its edit form, by id. */
    editing: string | null
    /** True between "signed in" and the first accounts response. */
    loading: boolean
    notifications: Notification[]
    profile: Profile
    /** Substring filter over the username column. */
    search: string
    /** `null` until `/api/admin/me` has answered — the app shows a splash. */
    signedIn: boolean | null
}
