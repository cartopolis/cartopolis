import type {
    Account,
    AccountRoles,
    AdminState,
    OwnedBuilding,
    LibraryEntry,
    Notification,
    PermissionInfo,
    PipelineCoverage,
    PipelineJob,
    PipelineLogLine,
    PlaceholderModel,
    Role,
    StoredAsset,
} from '@/types'

/**
 * What survives a reload — and, because `Store.save` filters against this shape,
 * the *only* thing that can.
 *
 * The session token is here on purpose: without it every refresh is a new login,
 * and the alternative (an HttpOnly cookie) needs the API to set one, which would
 * make the admin surface stateful in a way the game's own token is not. The token
 * is the same opaque, expiring, revocable string the game client persists in
 * `user.json`; `Sessions::revoke` invalidates it server-side on sign-out.
 */
export const persistentState: AdminState = {
    accounts: [],
    accountRoles: [] as AccountRoles[],
    adminTab: 'accounts',
    /*
     * None of the three below is persisted: what an operator may do is the
     * server's to say on every load, and a role grid from localStorage would
     * show permissions another operator has since taken away.
     */
    me: {admin: false, permissions: []},
    permissionCatalogue: [],
    roles: [],
    claims: [],
    overrides: null,
    /*
     * Not persisted, for the reason the account list is not: a catalogue rendered
     * from localStorage shows models an operator may have hidden from another tab,
     * and shows them as though the server had just confirmed them.
     */
    library: [],
    libraryAssets: [],
    libraryEditing: null,
    librarySearch: '',
    loading: false,
    buildingLimit: 0,
    /*
     * Persisted: the servers this browser signed in through on
     * `/signin/fediverse`, so the next time is one tap. Only host names, which
     * the person typed themselves; never a handle.
     */
    fediverseServers: [],
    myBuildings: [],
    notifications: [],
    placeholders: [],
    /*
     * None of the pipeline state is persisted, and the log is the reason worth
     * writing down: a run prints thousands of lines and localStorage is a five
     * megabyte budget shared with the session token. The rest is server state
     * that would be a lie after a reload — a job shown as running from
     * localStorage is a job that finished while the tab was closed.
     */
    pipelineConfig: null,
    pipelineCoverage: [],
    pipelineFollowing: '',
    pipelineJobs: [],
    pipelineLog: [],
    profile: {
        principal: '',
        token: '',
        username: '',
    },
    registration: null,
    search: '',
    signedIn: null,
    /*
     * Persisted, and for a reason the admin token's note does not cover: the
     * verification link is opened in whatever browser the mail was read in, and
     * the claim step comes *after* it. Losing this on a reload would strand
     * somebody one screen into their own sign-up with no way back except asking
     * for another link.
     */
    signup: {
        token: '',
        username: '',
    },
}

/**
 * Applied *after* the restored state, so everything here is deliberately reset on
 * every load. The account list is the point: a stale list rendered before the
 * first fetch shows accounts that may have been deleted from another tab, and it
 * would show them as authoritative.
 */
export const volatileState: Partial<AdminState> = {
    accounts: [] as Account[],
    accountRoles: [] as AccountRoles[],
    adminTab: 'accounts',
    me: {admin: false, permissions: []},
    permissionCatalogue: [] as PermissionInfo[],
    roles: [] as Role[],
    claims: [] as OwnedBuilding[],
    overrides: null,
    library: [] as LibraryEntry[],
    libraryAssets: [] as StoredAsset[],
    libraryEditing: null,
    librarySearch: '',
    loading: false,
    // Re-fetched from the server that holds it, for the same reason the account
    // list is: a house shown from localStorage is a claim that may have been
    // released, rendered as though it had just been confirmed.
    buildingLimit: 0,
    myBuildings: [],
    notifications: [] as Notification[],
    // Server state, so not persisted: a placeholder shown from localStorage is a
    // street another operator may have put back to its default.
    placeholders: [] as PlaceholderModel[],
    pipelineConfig: null,
    pipelineCoverage: [] as PipelineCoverage[],
    pipelineFollowing: '',
    pipelineJobs: [] as PipelineJob[],
    pipelineLog: [] as PipelineLogLine[],
    registration: null,
    search: '',
    signedIn: null,
}
