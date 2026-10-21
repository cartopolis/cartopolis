/**
 * One account, exactly as `auth::AccountRow` serialises it.
 *
 * There is no `pw_hash` here because there is none there — the server type omits
 * it deliberately (see its doc comment), so this interface is a description of
 * the wire rather than a filter applied to it.
 *
 * Checked, not only hand-synced: the Rust test `web_mirror::account_row` in
 * `crates/simulator/tests/integration/` serialises an `AccountRow` and fails
 * when its keys and this interface's fields differ.
 */
export interface Account {
    admin: boolean
    created_at: string
    disabled: boolean
    /**
     * Only self-registered accounts have one; an operator makes accounts without.
     * So `null` here is not "we didn't fetch it" — it is the account's vintage.
     */
    email: null | string
    /**
     * A registration still waiting on its address. Deliberately separate from
     * `disabled`: that is the operator's switch, this is the email gate, and an
     * account can be stopped by either.
     */
    email_pending: boolean
    id: string
    username: string
}

/** One address the register offered, with this server's answer about it. */
export interface BuildingAddress {
    address: string
    adresseerbaarobject_id: string
    /** False when somebody already owns the building it was claimed through. */
    available: boolean
    latitude: number
    longitude: number
    municipality: string
    /** The BAG address id: what a claim is keyed on. */
    nummeraanduiding_id: string
    postcode: null | string
}

/**
 * A building somebody owns, as `buildings::Ownership` serialises it. One
 * building has one owner; one account may own several.
 */
export interface OwnedBuilding {
    /** The address it was claimed through, and the one it is shown by. */
    address: string
    claimed_at: string
    latitude: number
    longitude: number
    nummeraanduiding_id: string
    /** The 3DBAG building id — the join to the geometry the game client draws. */
    pand_id: string
    postcode: null | string
    /** Present on the operator's listing only; your own buildings are yours. */
    username?: string
}

/**
 * The session a *new* account gets, which is not the admin session.
 *
 * Kept apart from `Profile` on purpose. Confirming an email hands back an
 * ordinary token for an ordinary account, and `App.init` validates the admin
 * token against `/api/admin/me` — writing one into the other would sign the new
 * player out of their own sign-up on the next reload, with a 401 as the
 * explanation.
 */
export interface SignupSession {
    token: string
    username: string
}

/** What the sign-up form asks the server before it draws itself. */
export interface RegistrationRules {
    /** Whether this server hands out buildings. Absent on simulators older than 2026-08. */
    houses_open?: boolean
    max_username_length: number
    min_password_length: number
    /** `false` on a server that does not take registrations. */
    open: boolean
    verification_valid_hours: number
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

/**
 * The data pipeline, as atlas exposes it (`crates/atlas/src/api.rs`,
 * `jobs.rs`, `coverage.rs`).
 *
 * Hand-synced like the account types above: these describe Rust structs, and no
 * compiler checks a TypeScript interface against one.
 */
export interface PipelineArea {
    bbox: number[]
    id: string
    km2: number
    label: string
}

export interface PipelineDataset {
    description: string
    id: string
    label: string
    /** Roughly how long a city-sized area takes, so the web UI can say so. */
    typicalMinutes: number
}

export interface PipelineConfig {
    areas: PipelineArea[]
    datasets: PipelineDataset[]
    /** The served directory a run writes into, shown so an operator can find it. */
    out: string
}

/**
 * One named total — "94,052 buildings".
 *
 * A list of pairs and not a `Record<string, number>`: the store is a deepsignal
 * proxy, and its types cannot describe an object with a string index signature,
 * so a record here fails to compile in the component that renders it rather than
 * here. `pipeline::Count` in atlas is the other side.
 */
export interface PipelineCount {
    label: string
    n: number
}

/** What the served tree holds right now, read from the coverage manifests. */
export interface PipelineCoverage {
    bbox: null | number[]
    cells: number
    counts: PipelineCount[]
    dataset: string
    label: string
    /** `null` when this dataset has never been extracted. */
    name: null | string
    updatedAt: null | number
}

export type JobStatus = 'queued' | 'running' | 'done' | 'failed' | 'cancelled'

export interface PipelineStep {
    counts?: PipelineCount[]
    dataset: string
    error?: string
    finishedAt?: number
    progress: {done?: number; phase?: string; total?: null | number}
    startedAt?: number
    status: JobStatus
}

export interface PipelineJob {
    area: string
    bbox: number[]
    by: string
    createdAt: number
    finishedAt?: number
    id: string
    startedAt?: number
    status: JobStatus
    steps: PipelineStep[]
}

export interface PipelineLogLine {
    at: number
    dataset: string
    text: string
}

/**
 * One entry in the shared model library, as `simulator::library::CuratorEntry`
 * serialises it.
 *
 * Note what is *per file* and what is not. The catalogue is keyed on `asset` —
 * the content hash the simulator stores the glTF under — because two people
 * uploading the same bench share one file. So `uploaders` is a list, and the
 * name and category here are the catalogue's, not any one uploader's.
 */
export interface LibraryEntry {
    /** The content hash: the catalogue key, and the download address. */
    asset: string
    /** The credit line a CC-BY model has to be shown with. Empty when none. */
    attribution: string
    byte_size: number
    category: string
    created_at: string
    /** How many inventories hold it. */
    held_by: number
    /** How many prims in the world wear it. */
    in_world: number
    /** `own`, `cc0`, `cc-by` or `other` — what the sharer stated. */
    licence: string
    name: string
    /** Who published it, which is not necessarily who uploaded it first. */
    shared_by: null | string
    tags: string[]
    triangles: number
    /** Everyone who has uploaded these exact bytes. */
    uploaders: string[]
    /** `shared` (published, unreviewed), `curated` (approved), `hidden` (taken down). */
    visibility: string
}

/**
 * What this server draws one kind of street object with, as
 * `simulator::library::PlaceholderModel` serialises it.
 *
 * Street objects — lampposts, bins, benches, parked cars — are drawn from a
 * register rather than placed by anyone, so there is nothing in the world to
 * select and change. This is the one statement an operator can make about them,
 * and it is world-wide: two people on one street must not see different streets.
 *
 * Guarded by the Rust test `web_mirror::placeholder_model` in
 * `crates/simulator/tests/integration/`, which compares its serialised keys
 * with these fields.
 */
export interface PlaceholderModel {
    /** The model's content hash, from the shared library. */
    asset: string
    /** A slug from `PLACEHOLDER_KINDS`. */
    kind: string
    /** Metres per model unit. */
    scale: number
    /** Which operator set it. */
    set_by: string
    /** Degrees, added to the angle the client computed for the object. */
    yaw_offset: number
}

/** A stored file and what still refers to it, from `/api/admin/assets`. */
export interface StoredAsset {
    byte_size: number
    created_at: string
    hash: string
    held_by: number
    in_world: number
    largest_texture: number
    nodes: number
    /** Whether it appears in the catalogue at all. */
    shared: boolean
    textures: number
    triangles: number
}

/**
 * One permission, as the simulator lists it (`GET /api/admin/permissions`,
 * `cartopolis_core::permissions`). The web UI keeps no list of its own — it
 * draws whatever the server says exists.
 */
export interface PermissionInfo {
    group: 'prims' | 'scripts' | 'buildings' | 'server'
    label: string
    player_default: boolean
    slug: string
}

/** A role: a named set of permissions (`roles::RoleRow`). */
export interface Role {
    built_in: boolean
    id: string
    /** How many accounts hold it explicitly; `player` is held by everyone. */
    members: number
    name: string
    permissions: string[]
}

/** The roles one account holds explicitly. A list rather than a map because
 * deepsignal does not type string-keyed records. */
export interface AccountRoles {
    id: string
    roles: string[]
}

/** What the signed-in operator may do (`/api/admin/me`). */
export interface Me {
    admin: boolean
    permissions: string[]
}

export type AdminTab =
    | 'accounts'
    | 'roles'
    | 'claims'
    | 'reports'
    | 'overrides'
    | 'library'
    | 'pipeline'
    | 'stats'
    | 'stats_world'
    | 'stats_web'
    | 'stats_places'

/*
 * Usage statistics, as the simulator's `stats::api` serialises them
 * (`docs/notes/usage-stats.md`). Days are UTC day numbers (unix seconds /
 * 86400) and hours unix hours, for the page to draw in the reader's zone.
 */

/** Different accounts in the world on one day, by the app they used. */
export interface StatsDayApps {
    all: number
    android: number
    chat: number
    day: number
    desktop: number
    unknown: number
    web: number
}

export interface StatsDayCount {
    day: number
    n: number
}

/** A 250 m square, or a public post's place; `people` is person-days. */
export interface StatsPlace {
    lat: number
    lon: number
    minutes: number
    people: number
}

export interface StatsOverview {
    accounts_total: number
    /** Null past the 90 days sessions are kept. */
    active_period: null | number
    active_prev: null | number
    active_today: number
    daily: StatsDayApps[]
    days: number
    funnel: {accounts: number; came_back: number; opened_world: number; signed_in: number; visitors: number}
    new_accounts: number
    new_accounts_prev: number
    online_now: number
    peak_today: number
    places: StatsPlace[]
    today: number
    visitors: number
    visitors_daily: StatsDayCount[]
    visitors_prev: number
}

export interface StatsEventTotal {
    daily: number[]
    kind: string
    n: number
    prev: number
}

export interface StatsWorld {
    apps: {build: string; client: string; people: number}[]
    /** Each week's new accounts and the share seen again in each week after; `back` is empty below 5. */
    cohorts: {back: number[]; size: number; week: number}[]
    days: number
    events: StatsEventTotal[]
    hours: {avg: number; hour: number; peak: number}[]
    longest_minutes: number
    median_minutes: number
    peak: number
    peak_hour: null | number
    refused: {attempts: number; build: string; client: string; devices: number; protocol: number; reason: string}[]
    sessions: number
    sessions_prev: number
}

export interface StatsRanked {
    value: string
    views: number
    visitors: number
}

export interface StatsWeb {
    /** Whole APK downloads, whatever site is chosen: the APK is on none of them. */
    apk_downloads: number
    apk_downloads_prev: number
    bots: number
    /** Two-letter codes; `unknown` while the server has no country table. */
    countries: StatsRanked[]
    daily: {day: number; views: number; visitors: number}[]
    days: number
    devices: StatsRanked[]
    languages: StatsRanked[]
    opened_world: number
    pages: StatsRanked[]
    site: null | string
    sources: StatsRanked[]
    views: number
    visitors: number
    visitors_prev: number
}

export interface StatsPlaces {
    days: number
    per_lat: number
    per_lon: number
    squares: {lat: number; lon: number; minutes: number; people: number; sx: number; sy: number}[]
}

/** A community's own numbers, for its owner and moderators. */
export interface CommunityStats {
    /** Null below 5 people. */
    active_members: null | number
    boosts_in: number
    call_minutes: number
    days: number
    followers: number
    followers_by_server: {n: number; server: string}[]
    followers_new: number
    joined: number
    left: number
    members: number
    messages: number
    messages_prev: number
    post_places: StatsPlace[]
    posts: number
    replies_in: number
    /** `writers` is null, and the counts 0, where fewer than 5 people wrote. */
    topics: {messages: number; messages_prev: number; name: string; topic: string; writers: null | number}[]
    visitors: number
    visitors_daily: StatsDayCount[]
    visitors_prev: number
    weeks: {joined: number; left: number; week: number}[]
}

/**
 * A report about a person, from the app's Report (`ReportUser` on the wire), as
 * `/api/admin/reports` lists it: `db::ReportRow` in the simulator. A report on a
 * line in a community's topic is not one of these; it goes to that community's
 * moderators through the chat.
 */
export interface UserReport {
    created_at: string
    id: number
    /** The line they were reported for, when the report quoted one. */
    message: string | null
    note: string
    reason: string
    /** Empty when the reporter's account has since been deleted. */
    reporter_username: string | null
    resolution: string | null
    resolved: boolean
    resolved_at: string | null
    subject_username: string
}

export interface AdminState extends Record<string, unknown> {
    accounts: Account[]
    /** The roles each account holds explicitly, by account id. */
    accountRoles: AccountRoles[]
    /** Which list the signed-in operator is looking at. */
    adminTab: AdminTab
    /** What the signed-in operator may do; decides which tabs exist. */
    me: Me
    /** Every permission the server knows, for the roles grid. */
    permissionCatalogue: PermissionInfo[]
    /** Every role on the server. */
    roles: Role[]
    /** The operator's view of who owns which building. */
    claims: OwnedBuilding[]
    /** Buildings whose survey shell is replaced; `null` until atlas answers. */
    overrides: BuildingOverride[] | null
    /** True between "signed in" and the first accounts response. */
    loading: boolean
    notifications: Notification[]
    profile: Profile
    /** Substring filter over the username column. */
    search: string
    /** The buildings the *signed-up* account owns. */
    myBuildings: OwnedBuilding[]
    /** How many one account may own, from the server; 0 until it has said. */
    buildingLimit: number
    /** `undefined` until `/api/register` has answered, `null` if it could not;
     *  the form waits rather than guessing. */
    registration: RegistrationRules | null | undefined
    /** `null` until `/api/admin/me` has answered — the app shows a splash. */
    signedIn: boolean | null
    /** The new account's own session, if this browser has just made one. */
    signup: SignupSession
    /** What the pipeline server offers; `null` until it has answered. */
    pipelineConfig: PipelineConfig | null
    /** What the served tree currently covers. */
    pipelineCoverage: PipelineCoverage[]
    /** Every run this process remembers, newest first. */
    pipelineJobs: PipelineJob[]
    /** The run the web UI is following, or `''`. */
    pipelineFollowing: string
    /** The followed run's log, as far as it has been fetched. */
    pipelineLog: PipelineLogLine[]
    /** The shared model library, including entries an operator has hidden. */
    library: LibraryEntry[]
    /** Every stored file, for the orphan listing. */
    libraryAssets: StoredAsset[]
    /** Which entry's row is expanded into its edit form, by asset hash. */
    libraryEditing: string | null
    /** Substring filter over the catalogue's names and tags. */
    librarySearch: string
    /** What this server draws its street objects with. */
    placeholders: PlaceholderModel[]
}

/** Where a replacement building stands (`overrides::Placement`). An absent
 * lat/lng puts it on the register outline's centre. */
export interface OverridePlacement {
    base_m: number
    heading_deg: number
    lat: number | null
    lng: number | null
    scale: number
}

/** A building whose survey shell is replaced (`overrides::Override`). Deleting
 * it brings the 3DBAG building back. */
export interface BuildingOverride {
    author: string
    /** `[x, y]` at z14 — a list rather than a tuple, which deepsignal does not type. */
    cell: number[] | null
    collision: 'parts' | 'walls' | 'none'
    interior: 'none' | 'design'
    name: string
    pand: string
    placement: OverridePlacement
    revision: number
    source: {kind: 'stadium'; pitch: number[][]} | {kind: 'model'; sha256: string; triangles: number}
}

/** What a save compiled to (`overrides::Summary`). */
export interface OverrideSummary {
    cell: number[]
    cell_found: boolean
    elements: number
    triangles: number
}

/**
 * A community's public posts (`cartopolis_core::posts`,
 * `docs/notes/fediverse.md`): the half of a community anybody may read, which
 * the simulator also publishes to the fediverse. Served by
 * `/api/communities/{id}/posts` and `/api/posts/{id}`.
 *
 * Guarded by the Rust tests `web_mirror::post_dto` and `web_mirror::public_info`
 * in `crates/simulator/tests/integration/`, which compare their serialised keys
 * with these fields.
 */
export interface PostView {
    altitude: number
    pitch: number
    yaw: number
}

export interface PostPlace {
    latitude: number
    longitude: number
    name?: string
    view?: PostView
}

export interface PostAuthor {
    /** What to show: a display name from elsewhere, or the username here. */
    name: string
    /** `name@server`, here or elsewhere. */
    address: string
    /** Whether they have an account on this server. */
    local: boolean
}

export interface PostDto {
    id: string
    community: string
    /** The post a reply is under. */
    parent?: string
    author: PostAuthor
    /** The fediverse source the community follows that brought it. */
    via?: string
    text: string
    /** A hash in the picture store. */
    picture?: string
    /** Its snapshot of the world, made in the app, beside the photo. */
    snapshot?: string
    /** Where it was written; most posts from elsewhere have none. */
    place?: PostPlace
    /** The post's own web page, for one from another server. */
    url?: string
    created_ms: number
    edited_ms?: number
    /** Replies under a post; 0 on a reply. */
    replies: number
    /** Whether the caller wrote it (and so may edit or delete it). */
    mine: boolean
    /** Likes from other servers: Mastodon's favourites, Lemmy's upvotes. */
    likes: number
    /** Lemmy's downvotes. */
    dislikes: number
}

/** A report about a post, sent by another server for the moderators. */
export interface PostReport {
    id: string
    post: PostDto
    /** An account's address, or a server reporting for one of its people. */
    reporter: string
    reason: string
    created_ms: number
}

/** A person's public posts, for their page (`/@name`). */
export interface PersonPosts {
    address: string
    picture?: string
    posts: PostDto[]
}

/** Where to follow an address from somebody's own fediverse server. */
export interface FollowLink {
    url: string
    /** What their server runs, when it says. */
    software?: string
}

export interface PostThread {
    post: PostDto
    replies: PostDto[]
}

export type WhoPosts = 'members' | 'moderators'

export interface PublicSettings {
    enabled: boolean
    /** The fediverse address before `@server`; chosen once, never changed. */
    handle?: string
    who_posts: WhoPosts
}

export interface FollowDto {
    address: string
    name: string
    kind: 'account' | 'community'
    accepted: boolean
}

/** A community as its public page shows it to anybody. */
export interface PublicCommunity {
    id: string
    name: string
    about: string
    picture?: string
}

export interface PublicInfo {
    settings: PublicSettings
    /** `handle@server`, once there is a handle. */
    address?: string
    followers: number
    follows: FollowDto[]
    may_post: boolean
    may_moderate: boolean
    federating: boolean
    /** The community itself, while posts are on. */
    community?: PublicCommunity
    /** Reports from other servers waiting for a moderator; 0 for anybody else. */
    open_reports: number
}

/**
 * One of the signed-in person's access tokens, as `/api/tokens` lists it: what a
 * bot or a script signs in with instead of a password (`docs/notes/access-tokens.md`).
 * Everything but the secret, which only `POST /api/tokens` returns, once.
 *
 * Guarded by the Rust test `web_mirror::access_token` in
 * `crates/simulator/tests/integration/`, which compares its serialised keys
 * with these fields.
 */
export interface AccessToken {
    /** Unix seconds. */
    created_at: number
    /** Carries the account's elevated permissions (running the server, editing others' things). */
    elevated: boolean
    /** Unix seconds; null for a token that lasts until it is revoked. */
    expires_at: number | null
    /** What the revoke route takes; not a secret. */
    id: string
    label: string
    /** Unix seconds, to the minute; null if it was never used. */
    last_used: number | null
}
