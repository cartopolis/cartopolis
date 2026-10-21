import m from '@garage44/mithril'
import {Button, FieldSegmented, FieldText} from '@garage44/mithril/components'

import {signupApi} from '@/app'
import {ApiError} from '@/lib/api'
import type {CommunityView} from '@/lib/chat'
import {Icon} from '@/lib/icons'
import {parsePlace, placeParam, suggestHandle} from '@/lib/posts'
import type {FollowLink, PostDto, PostPlace, PostReport, PostThread, PublicInfo, WhoPosts} from '@/types'

/**
 * A community's public posts (`docs/notes/fediverse.md`,
 * `docs/mockups/communities/v4-public-posts.html`).
 *
 * The other half of a community page from its topics: public where they are
 * encrypted, and on the fediverse where they never are. Plain HTTP against the
 * simulator (`/api/communities/{id}/posts`), not the chat worker, because there
 * is nothing to seal: the server stores posts readable on purpose, to send
 * them to other servers.
 *
 * Laid out as a feed, as Mastodon's is: a box to write in at the top, then
 * each post as a card with its author, its words, its picture and its place,
 * and its replies opening underneath it rather than on a page of their own.
 * What is done rarely (hide, block, delete) sits behind a ⋯ menu.
 *
 * A post always has a place. The app takes it from the camera; here it is the
 * community's own place, where this browser is, or a Cartopolis place link
 * pasted in (Copy link in the app, or any `/go?place=` link). A post's place
 * opens in World, flown to the view it was written from.
 */

export type Tab = 'posts' | 'topics'

interface Draft {
    community: string
    text: string
    /** What was typed into the place field: a link or "lat, lng". */
    where: string
    place: PostPlace | null
    picture: string | null
    /** Whether the place field is showing. */
    placing: boolean
}

/**
 * Where a card is drawn: which community, and whether the reader moderates it
 * and is signed in. A community page passes its own; the public pages
 * (`pages/public`) a reader who may be nobody.
 */
export interface Where {
    id: string
    mod: boolean
    signedIn: boolean
}

/** What the page lends this view: pictures, faces, and the World frame. */
export interface PostsCtx {
    pictureUrl: (hash: string | null | undefined) => string | null
    avatar: (address: string, size: number) => m.Children
    /** Open World at a place, given as a link's `place=` value. */
    onMap: (place: string) => void
}

/** Everything the posts half holds, across communities. */
const st = {
    busy: false,
    draft: null as Draft | null,
    /** The post being edited, and its words so far. */
    editing: null as {id: string; text: string} | null,
    error: '',
    feed: new Map<string, PostDto[]>(),
    follow: '',
    handle: '',
    info: new Map<string, PublicInfo>(),
    loaded: null as string | null,
    reply: '',
    /** Reports from other servers, per community, for its moderators. */
    reports: new Map<string, PostReport[]>(),
    settingsOpen: false,
    tab: new Map<string, Tab>(),
    thread: null as PostThread | null,
    threadOpen: null as string | null,
}

function failed(e: unknown): void {
    st.busy = false
    st.error = e instanceof ApiError ? e.message : `Not done: ${(e as Error).message}`
    m.redraw()
}

/** What has been read of a community's public half, for the public pages. */
export function readOf(id: string): {info: PublicInfo | undefined; feed: PostDto[]} {
    return {feed: st.feed.get(id) ?? [], info: st.info.get(id)}
}

/** Read a community's public half and its newest posts. */
export async function load(id: string): Promise<void> {
    try {
        const [info, feed] = await Promise.all([
            signupApi.get<PublicInfo>(`/api/communities/${id}/public`),
            signupApi.get<PostDto[]>(`/api/communities/${id}/posts`),
        ])
        st.info.set(id, info)
        st.feed.set(id, feed)
        if (!st.handle && info.settings.handle) st.handle = info.settings.handle
        if (info.open_reports > 0) st.reports.set(id, await signupApi.get<PostReport[]>(`/api/communities/${id}/reports`))
        else st.reports.delete(id)
    } catch (e) {
        failed(e)
    }
    m.redraw()
}

/** Open (or close) a post's replies under it. */
export async function toggleThread(post: string): Promise<void> {
    if (st.threadOpen === post) {
        st.threadOpen = null
        st.thread = null
        return
    }
    st.threadOpen = post
    st.thread = null
    st.reply = ''
    try {
        st.thread = await signupApi.get<PostThread>(`/api/posts/${post}`)
    } catch (e) {
        failed(e)
    }
    m.redraw()
}

export function ago(ms: number): string {
    const s = Math.max(0, (Date.now() - ms) / 1000)
    if (s < 60) return 'now'
    if (s < 3600) return `${Math.floor(s / 60)} min`
    if (s < 86400) return `${Math.floor(s / 3600)} h`
    if (s < 604800) return `${Math.floor(s / 86400)} d`
    return new Date(ms).toLocaleDateString()
}

/** A picked picture, at most 1600 px on its longest edge, as a JPEG in the picture store. */
async function uploadPicture(file: File, d: Draft): Promise<void> {
    st.busy = true
    m.redraw()
    try {
        const bitmap = await createImageBitmap(file)
        const scale = Math.min(1, 1600 / Math.max(bitmap.width, bitmap.height))
        const canvas = document.createElement('canvas')
        canvas.width = Math.round(bitmap.width * scale)
        canvas.height = Math.round(bitmap.height * scale)
        canvas.getContext('2d')!.drawImage(bitmap, 0, 0, canvas.width, canvas.height)
        const blob = await new Promise<Blob>((ok, fail) =>
            canvas.toBlob((b) => (b ? ok(b) : fail(new Error('could not encode it'))), 'image/jpeg', 0.85))
        const {hash} = await signupApi.upload<{hash: string}>('/api/pictures', await blob.arrayBuffer(), 'application/octet-stream')
        d.picture = hash
        st.busy = false
    } catch (e) {
        failed(e)
    }
    m.redraw()
}

/** Which half of a community page is showing; the posts, unless chosen otherwise. */
export function tabOf(id: string): Tab {
    return st.tab.get(id) ?? 'posts'
}

/** The two halves of a community page, side by side. */
export function Tabs(id: string, topics: number) {
    if (st.loaded !== id) {
        st.loaded = id
        st.threadOpen = null
        st.thread = null
        st.settingsOpen = false
        st.handle = ''
        st.error = ''
        st.draft = null
        void load(id)
    }
    return (
        <div class="post-tabs">
            <FieldSegmented
                model={{
                    get value() {
                        return tabOf(id)
                    },
                    set value(t: Tab) {
                        st.tab.set(id, t)
                    },
                }}
                options={[
                    {label: 'Posts · public', value: 'posts' as Tab},
                    {label: `Topics · ${topics} · encrypted`, value: 'topics' as Tab},
                ]}
                size="sm"
            />
        </div>
    )
}

/** The public half of a community page. */
export function Posts(community: CommunityView, me: string, ctx: PostsCtx) {
    const id = community.id
    const mod = community.role === 'owner' || community.role === 'moderator'
    const info = st.info.get(id)
    const error = st.error && (
        <p class="post-error">
            {st.error}
            <button aria-label="Dismiss" onclick={() => (st.error = '')} type="button">{Icon('close')}</button>
        </p>
    )
    if (!info) return [error, <p class="chat-hint">Reading the posts…</p>]
    if (!info.settings.enabled) {
        return [
            error,
            <div class="post-empty">
                <b>No public posts yet</b>
                <p>
                    With them on, anybody can read what members post here: on the map, and on the fediverse (Mastodon, Lemmy
                    and others), where people can follow the community and reply.
                    {mod ? '' : ' Its owner or a moderator can switch them on.'}
                </p>
            </div>,
            mod && Settings(community, info),
        ]
    }
    const feed = st.feed.get(id) ?? []
    const where: Where = {id, mod, signedIn: true}
    return [
        <div class="post-bar">
            <span class="post-public" title="Public: anybody can read these, here and on other servers">
                {Icon('public')} Public · {info.followers} follower{info.followers === 1 ? '' : 's'} on the fediverse
                {info.address && <span class="post-dim"> · @{info.address}</span>}
            </span>
            <span class="post-bar-tools">
                <button aria-label="Read again" class="post-icon" onclick={() => void load(id)} title="Read again" type="button">{Icon('refresh')}</button>
                {mod && (
                    <button
                        aria-label="Settings"
                        class={st.settingsOpen ? 'post-icon is-on' : 'post-icon'}
                        onclick={() => (st.settingsOpen = !st.settingsOpen)}
                        title="Settings"
                        type="button"
                    >
                        {Icon('settings')}
                    </button>
                )}
            </span>
        </div>,
        error,
        info.address && info.federating && FollowFrom(info.address, true),
        mod && st.settingsOpen && Settings(community, info),
        mod && Reports(id),
        info.may_post && Composer(community, info, me, ctx),
        feed.length === 0 && (
            <div class="post-empty">
                <b>Nothing posted yet</b>
                <p>
                    {info.may_post
                        ? 'Write the first: it is pinned on the map where it is about.'
                        : 'Posts from members and from the fediverse appear here.'}
                </p>
            </div>
        ),
        feed.map((p) => Card(where, p, ctx, true)),
    ]
}

/** What the box asking for somebody's own server holds, per address. */
const fromServer = new Map<string, string>()

/**
 * "Follow from your own server": somebody on Mastodon, Lemmy or elsewhere
 * types their server, and is sent to the page there that follows this address
 * — Mastodon's follow screen, Lemmy's search, Misskey's — which the simulator
 * picks by asking their server what it runs (`/api/fediverse/follow-link`).
 */
export function FollowFrom(address: string, group: boolean) {
    const typed = fromServer.get(address) ?? ''
    const go = async () => {
        const server = typed.trim()
        if (!server) return
        try {
            const link = await signupApi.get<FollowLink>(
                `/api/fediverse/follow-link?server=${encodeURIComponent(server)}&address=${encodeURIComponent(address)}`,
            )
            window.open(link.url, '_blank', 'noopener')
        } catch (e) {
            failed(e)
        }
    }
    return (
        <form
            class="post-follow-from"
            key={`from-${address}`}
            onsubmit={(e: Event) => {
                e.preventDefault()
                void go()
            }}
        >
            <span class="post-dim">
                Follow {group ? `@${address} (on Lemmy: !${address})` : `@${address}`} from your own server
            </span>
            <span class="post-follow-from-row">
                <input
                    aria-label="Your server"
                    oninput={(e: Event) => fromServer.set(address, (e.target as HTMLInputElement).value)}
                    placeholder="mastodon.social, lemmy.world, …"
                    value={typed}
                />
                <Button disabled={!typed.trim()} size="sm" type="submit">Follow</Button>
                <button
                    class="post-icon"
                    onclick={() => void navigator.clipboard?.writeText(`@${address}`)}
                    title="Copy the address"
                    type="button"
                >
                    {Icon('content_copy')}
                </button>
            </span>
        </form>
    )
}

/** Reports from other servers about posts here, for the moderators. */
function Reports(id: string) {
    const reports = st.reports.get(id) ?? []
    if (reports.length === 0) return null
    const resolve = async (report: string, action: 'hide' | 'dismiss') => {
        try {
            await signupApi.post(`/api/communities/${id}/reports/${report}`, {action})
            await load(id)
        } catch (e) {
            failed(e)
        }
    }
    return (
        <div class="post-reports">
            <h3 class="chat-pane-h">
                {Icon('flag')} {reports.length} report{reports.length === 1 ? '' : 's'} from other servers
            </h3>
            {reports.map((r) => (
                <div class="post-report" key={r.id}>
                    <p class="post-dim is-public">{r.reporter} · {ago(r.created_ms)}</p>
                    {r.reason && <p class="post-report-reason">{r.reason}</p>}
                    <blockquote class="post-report-post">
                        <b>{r.post.author.address}</b> {r.post.text}
                    </blockquote>
                    <span class="post-report-actions">
                        <Button onclick={() => void resolve(r.id, 'hide')} size="sm">Hide the post</Button>
                        <Button onclick={() => void resolve(r.id, 'dismiss')} size="sm" variant="ghost">Leave it</Button>
                    </span>
                </div>
            ))}
        </div>
    )
}

/** A post's place, as a chip that opens it in World. */
function PlaceChip(place: PostPlace | undefined, ctx: PostsCtx) {
    if (!place) return <span class="post-noplace">Not on the map</span>
    return (
        <button
            class="post-place-chip"
            onclick={() => ctx.onMap(placeParam(place))}
            title="Open in World, seen from where it was written"
            type="button"
        >
            {Icon('location_on')} {place.name ?? 'Show on the map'}
        </button>
    )
}

export function Card(where: Where, p: PostDto, ctx: PostsCtx, top: boolean) {
    const mod = where.mod
    const url = ctx.pictureUrl(p.picture)
    const snapshot = ctx.pictureUrl(p.snapshot)
    const open = top && st.threadOpen === p.id
    const act = async (run: () => Promise<unknown>) => {
        try {
            await run()
            if (st.threadOpen === p.id) st.threadOpen = null
            await load(where.id)
        } catch (e) {
            failed(e)
        }
    }
    const editing = st.editing?.id === p.id ? st.editing : null
    const saveEdit = async () => {
        const text = editing?.text.trim()
        if (!text || st.busy) return
        st.busy = true
        try {
            const changed = await signupApi.put<PostDto>(`/api/posts/${p.id}`, {text})
            Object.assign(p, changed)
            st.editing = null
            st.busy = false
        } catch (e) {
            failed(e)
        }
        m.redraw()
    }
    const menu = [
        p.mine && (
            <button onclick={() => (st.editing = {id: p.id, text: p.text})} type="button">
                Edit
                <small>Other servers get the change, as they do Mastodon's own edits</small>
            </button>
        ),
        p.mine && (
            <button onclick={() => void act(() => signupApi.delete(`/api/posts/${p.id}`))} type="button">
                Delete
                <small>Here and, as far as they listen, on other servers</small>
            </button>
        ),
        mod && !p.mine && (
            <button onclick={() => void act(() => signupApi.post(`/api/posts/${p.id}/hide`))} type="button">
                Hide from this community
            </button>
        ),
        mod && !p.author.local && (
            <button
                onclick={() =>
                    void act(() => signupApi.post(`/api/communities/${where.id}/blocks`, {address: p.author.address}))}
                type="button"
            >
                Block {p.author.address}
                <small>They can't post here again, and what they wrote is hidden</small>
            </button>
        ),
        !p.author.local && p.url && (
            <a href={p.url} rel="noopener" target="_blank">Open where it was posted</a>
        ),
    ].filter(Boolean)
    return (
        <article class={top ? 'post-card' : 'post-card is-reply'} key={p.id}>
            <div class="post-face">{ctx.avatar(p.author.address, top ? 36 : 28)}</div>
            <div class="post-main">
                <header class="post-head">
                    <span class="post-who">
                        <b>{p.author.name}</b>
                        <span class={p.author.local ? 'post-dim' : 'post-dim is-public'}>
                            {p.author.local ? 'member' : `@${p.author.address}`}
                        </span>
                    </span>
                    <span class="post-dim" title={new Date(p.created_ms).toLocaleString()}>
                        {ago(p.created_ms)}
                        {p.edited_ms && <span title={`Edited ${new Date(p.edited_ms).toLocaleString()}`}> · edited</span>}
                    </span>
                    {menu.length > 0 && (
                        <details class="post-menu">
                            <summary aria-label="More" title="More">{Icon('more_horiz')}</summary>
                            <div class="post-menu-list">{menu}</div>
                        </details>
                    )}
                </header>
                {p.via && <div class="post-via">via {p.via}</div>}
                {editing ? (
                    <form
                        class="post-edit"
                        onsubmit={(e: Event) => {
                            e.preventDefault()
                            void saveEdit()
                        }}
                    >
                        <textarea
                            maxlength={500}
                            oninput={(e: Event) => (editing.text = (e.target as HTMLTextAreaElement).value)}
                            rows={3}
                            value={editing.text}
                        />
                        <span class="post-edit-actions">
                            <Button onclick={() => (st.editing = null)} size="sm" variant="ghost">Cancel</Button>
                            <Button disabled={!editing.text.trim() || st.busy} size="sm" type="submit">Save</Button>
                        </span>
                    </form>
                ) : <p class="post-text">{p.text}</p>}
                {snapshot && (
                    <img alt="A snapshot of the map where this was written" class="post-polaroid" loading="lazy" src={snapshot} />
                )}
                {url && <img alt="" class="post-picture" loading="lazy" src={url} />}
                {top && (
                    <footer class="post-foot">
                        {PlaceChip(p.place, ctx)}
                        <button
                            class={open ? 'post-replies is-on' : 'post-replies'}
                            onclick={() => void toggleThread(p.id)}
                            type="button"
                        >
                            {Icon('comment')} {p.replies} {p.replies === 1 ? 'reply' : 'replies'}
                        </button>
                        {Votes(p)}
                    </footer>
                )}
                {!top && Votes(p)}
                {open && Replies(where, ctx)}
            </div>
        </article>
    )
}

/** Likes and votes from other servers, when there are any. */
function Votes(p: PostDto) {
    if (!p.likes && !p.dislikes) return null
    const title = 'From other servers: favourites on Mastodon, votes on Lemmy'
    return (
        <span class="post-votes" title={title}>
            {Icon('favorite')} {p.likes}
            {p.dislikes > 0 && <> · {Icon('thumb_down')} {p.dislikes}</>}
        </span>
    )
}

/** A post's replies, under it, with a box to answer in for somebody signed in. */
function Replies(where: Where, ctx: PostsCtx) {
    const t = st.thread
    if (!t) return <p class="post-dim post-loading">Reading the replies…</p>
    return (
        <div class="post-thread">
            {t.replies.map((r) => Card(where, r, ctx, false))}
            {!where.signedIn && <p class="post-dim">Sign in, or reply from your own fediverse server.</p>}
            {where.signedIn && <form
                class="post-reply"
                onsubmit={async (e: Event) => {
                    e.preventDefault()
                    const text = st.reply.trim()
                    if (!text || st.busy) return
                    st.busy = true
                    try {
                        const reply = await signupApi.post<PostDto>(`/api/posts/${t.post.id}/replies`, {text})
                        t.replies.push(reply)
                        t.post.replies += 1
                        const inFeed = st.feed.get(where.id)?.find((x) => x.id === t.post.id)
                        if (inFeed) inFeed.replies = t.post.replies
                        st.reply = ''
                        st.busy = false
                    } catch (err) {
                        failed(err)
                    }
                    m.redraw()
                }}
            >
                <textarea
                    maxlength={500}
                    oninput={(e: Event) => (st.reply = (e.target as HTMLTextAreaElement).value)}
                    placeholder="Reply in public"
                    rows={1}
                    value={st.reply}
                />
                <Button disabled={!st.reply.trim() || st.busy} size="sm" type="submit">Reply</Button>
            </form>}
        </div>
    )
}

/** The box at the top of the feed: one line until it is used, then the whole post. */
function Composer(community: CommunityView, info: PublicInfo, me: string, ctx: PostsCtx) {
    const d = st.draft?.community === community.id ? st.draft : null
    const start = () => {
        const p = community.settings.place
        st.draft = {
            community: community.id,
            picture: null,
            place: p ? {latitude: p.latitude, longitude: p.longitude, name: p.name} : null,
            placing: !p,
            text: '',
            where: '',
        }
    }
    if (!d) {
        return (
            <button class="post-compose is-folded" onclick={start} type="button">
                <span class="post-face">{ctx.avatar(me, 36)}</span>
                <span class="post-compose-hint">Write a public post…</span>
            </button>
        )
    }
    const url = ctx.pictureUrl(d.picture)
    const reach = info.federating
        ? `Public. On this page and the map, and sent to ${info.followers} follower${info.followers === 1 ? '' : 's'} elsewhere, under your own address.`
        : 'Public. On this page and the map, for anybody on this server.'
    const ready = !!d.place && !!d.text.trim() && !st.busy
    const send = async () => {
        if (!ready || !d.place) return
        st.busy = true
        try {
            const post = await signupApi.post<PostDto>(`/api/communities/${community.id}/posts`, {
                picture: d.picture ?? undefined,
                place: d.place,
                text: d.text.trim(),
            })
            st.feed.set(community.id, [post, ...(st.feed.get(community.id) ?? [])])
            st.draft = null
            st.busy = false
        } catch (e) {
            failed(e)
        }
        m.redraw()
    }
    return (
        <div class="post-compose">
            <span class="post-face">{ctx.avatar(me, 36)}</span>
            <div class="post-main">
                <textarea
                    autofocus
                    class="post-compose-text"
                    maxlength={500}
                    oninput={(e: Event) => (d.text = (e.target as HTMLTextAreaElement).value)}
                    placeholder="What's happening here?"
                    rows={3}
                    value={d.text}
                />
                {url && (
                    <div class="post-compose-picture">
                        <img alt="" class="post-picture" src={url} />
                        <button aria-label="Remove the picture" onclick={() => (d.picture = null)} type="button">{Icon('close')}</button>
                    </div>
                )}
                {d.placing && (
                    <div class="post-placing">
                        <FieldText
                            model={{
                                get value() {
                                    return d.where
                                },
                                set value(v: string) {
                                    d.where = v
                                    const p = parsePlace(v)
                                    if (p) {
                                        d.place = p
                                        d.placing = false
                                    }
                                },
                            }}
                            placeholder="Paste a place link from the app (Copy link), or lat, lng"
                        />
                        <div class="post-placing-choices">
                            {community.settings.place && (
                                <button
                                    onclick={() => {
                                        const p = community.settings.place!
                                        d.place = {latitude: p.latitude, longitude: p.longitude, name: p.name}
                                        d.placing = false
                                    }}
                                    type="button"
                                >
                                    The community's place
                                </button>
                            )}
                            <button
                                onclick={() =>
                                    navigator.geolocation?.getCurrentPosition(
                                        (pos) => {
                                            d.place = {latitude: pos.coords.latitude, longitude: pos.coords.longitude}
                                            d.placing = false
                                            m.redraw()
                                        },
                                        () => failed(new Error('this browser would not say where it is')),
                                    )}
                                type="button"
                            >
                                {Icon('my_location')} Where I am
                            </button>
                        </div>
                    </div>
                )}
                <div class="post-compose-tools">
                    <button
                        class={d.place ? 'post-place-chip' : 'post-place-chip is-missing'}
                        onclick={() => (d.placing = !d.placing)}
                        title="Where it is about"
                        type="button"
                    >
                        {Icon('location_on')} {d.place ? (d.place.name ?? `${d.place.latitude.toFixed(4)}, ${d.place.longitude.toFixed(4)}`) : 'Add a place'}
                    </button>
                    <label class="post-tool" title="Add a photo">
                        {Icon('image')} Photo
                        <input
                            accept="image/*"
                            hidden
                            onchange={(e: Event) => {
                                const file = (e.target as HTMLInputElement).files?.[0]
                                if (file) void uploadPicture(file, d)
                            }}
                            type="file"
                        />
                    </label>
                    <span class="post-count">{500 - d.text.length}</span>
                    <Button onclick={() => (st.draft = null)} size="sm" variant="ghost">Cancel</Button>
                    <Button disabled={!ready} onclick={() => void send()} size="sm">Post</Button>
                </div>
                <p class="post-reach">{reach}</p>
            </div>
        </div>
    )
}

function Settings(community: CommunityView, info: PublicInfo) {
    const id = community.id
    const first = info.settings.handle == null
    if (first && !st.handle) st.handle = suggestHandle(community.settings.name)
    const save = async (enabled: boolean, who: WhoPosts) => {
        try {
            const next = await signupApi.post<PublicInfo>(`/api/communities/${id}/public`, {
                enabled,
                handle: first ? st.handle.trim() : undefined,
                who_posts: who,
            })
            st.info.set(id, next)
            await load(id)
        } catch (e) {
            failed(e)
        }
    }
    return (
        <div class="post-settings">
            {first ? (
                <FieldText
                    help={`People elsewhere find it as @${st.handle.trim()}@${community.home}. It can't be changed later: their servers know the community by it.`}
                    label="The community's fediverse address"
                    model={{
                        get value() {
                            return st.handle
                        },
                        set value(v: string) {
                            st.handle = v
                        },
                    }}
                />
            ) : (
                <p><b>@{info.address}</b></p>
            )}
            <FieldSegmented
                label="Who can post"
                model={{
                    get value() {
                        return info.settings.who_posts
                    },
                    set value(w: WhoPosts) {
                        void save(info.settings.enabled, w)
                    },
                }}
                options={[
                    {label: 'Every member', value: 'members' as WhoPosts},
                    {label: 'Moderators only', value: 'moderators' as WhoPosts},
                ]}
                size="sm"
            />
            <Button onclick={() => void save(!info.settings.enabled, info.settings.who_posts)} size="sm" variant={info.settings.enabled ? 'ghost' : 'primary'}>
                {info.settings.enabled ? 'Switch public posts off' : 'Switch public posts on'}
            </Button>
            {info.settings.enabled && info.federating && (
                <>
                    <h3 class="chat-pane-h">This community follows</h3>
                    {info.follows.length === 0 && (
                        <p class="chat-hint">Nothing yet. Their posts would appear here, marked with where they came from.</p>
                    )}
                    {info.follows.map((f) => (
                        <div class="post-follow" key={f.address}>
                            <span class="is-public">{f.address}</span>
                            <span class="chat-sub">{f.kind}{f.accepted ? '' : ' · waiting for their server'}</span>
                            <button
                                onclick={async () => {
                                    try {
                                        st.info.set(id, await signupApi.delete<PublicInfo>(`/api/communities/${id}/follows/${encodeURIComponent(f.address)}`))
                                    } catch (e) {
                                        failed(e)
                                    }
                                    m.redraw()
                                }}
                                title="Stop following"
                                type="button"
                            >
                                {Icon('close')}
                            </button>
                        </div>
                    ))}
                    <form
                        class="chat-inline"
                        onsubmit={async (e: Event) => {
                            e.preventDefault()
                            const address = st.follow.trim()
                            if (!address.includes('@') && !address.startsWith('https://')) return
                            try {
                                st.info.set(id, await signupApi.post<PublicInfo>(`/api/communities/${id}/follows`, {address}))
                                st.follow = ''
                            } catch (err) {
                                failed(err)
                            }
                            m.redraw()
                        }}
                    >
                        <FieldText
                            model={{
                                get value() {
                                    return st.follow
                                },
                                set value(v: string) {
                                    st.follow = v
                                },
                            }}
                            placeholder="groningen@feddit.nl, someone@mastodon.social, or a relay's https:// address"
                        />
                        <Button size="sm" type="submit">Follow</Button>
                    </form>
                </>
            )}
        </div>
    )
}
