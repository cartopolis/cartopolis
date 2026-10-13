import m from '@garage44/mithril'
import {Button, FieldSegmented, FieldText, FieldTextarea} from '@garage44/mithril/components'

import {signupApi} from '@/app'
import {ApiError} from '@/lib/api'
import type {CommunityView} from '@/lib/chat'
import {parsePlace, placeLink, suggestHandle} from '@/lib/posts'
import type {PostDto, PostPlace, PostThread, PublicInfo, WhoPosts} from '@/types'

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
 * A post always has a place. The app takes it from the camera; here it is the
 * community's own place, where this browser is, or a Cartopolis place link
 * pasted in (Copy link in the app, or any `/go?place=` link).
 */

export type Tab = 'posts' | 'topics'

interface Draft {
    community: string
    text: string
    /** What was typed into the place field: a link or "lat, lng". */
    where: string
    place: PostPlace | null
    picture: string | null
}

/** Everything the posts half holds, across communities. */
const st = {
    busy: false,
    draft: null as Draft | null,
    error: '',
    feed: new Map<string, PostDto[]>(),
    follow: '',
    handle: '',
    info: new Map<string, PublicInfo>(),
    loaded: null as string | null,
    reply: '',
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

/** Read a community's public half and its newest posts. */
async function load(id: string): Promise<void> {
    try {
        const [info, feed] = await Promise.all([
            signupApi.get<PublicInfo>(`/api/communities/${id}/public`),
            signupApi.get<PostDto[]>(`/api/communities/${id}/posts`),
        ])
        st.info.set(id, info)
        st.feed.set(id, feed)
        if (!st.handle && info.settings.handle) st.handle = info.settings.handle
    } catch (e) {
        failed(e)
    }
    m.redraw()
}

async function openThread(post: string): Promise<void> {
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

function ago(ms: number): string {
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

type PictureUrl = (hash: string | null | undefined) => string | null

/** The public half of a community page. */
export function Posts(community: CommunityView, pictureUrl: PictureUrl) {
    const id = community.id
    const mod = community.role === 'owner' || community.role === 'moderator'
    const info = st.info.get(id)
    const error = st.error && (
        <p class="chat-hint is-error">
            {st.error} <button onclick={() => (st.error = '')} type="button">×</button>
        </p>
    )
    if (st.draft?.community === id) return [error, Composer(community, st.draft, pictureUrl)]
    if (st.threadOpen) return [error, Thread(community, pictureUrl)]
    if (!info) return [error, <p class="chat-hint">Reading the posts…</p>]
    if (!info.settings.enabled) {
        return [
            error,
            <p class="chat-hint">
                This community has no public posts. With them on, anybody can read what members post here, on the map and on
                the fediverse (Mastodon, Lemmy and others), and reply.
                {mod ? '' : ' Its owner or a moderator can switch them on.'}
            </p>,
            mod && Settings(community, info),
        ]
    }
    const feed = st.feed.get(id) ?? []
    return [
        error,
        <p class="post-reach">
            Anyone can read these, on any server · {info.followers} follower{info.followers === 1 ? '' : 's'}
            {info.address && <span class="chat-sub"> · @{info.address}</span>}
        </p>,
        <div class="post-actions">
            {info.may_post && (
                <Button
                    onclick={() => {
                        const p = community.settings.place
                        st.draft = {
                            community: id,
                            picture: null,
                            place: p ? {latitude: p.latitude, longitude: p.longitude, name: p.name} : null,
                            text: '',
                            where: '',
                        }
                    }}
                    size="sm"
                >
                    Write a post
                </Button>
            )}
            <Button onclick={() => void load(id)} size="sm" variant="ghost">Refresh</Button>
            {mod && (
                <Button onclick={() => (st.settingsOpen = !st.settingsOpen)} size="sm" variant="ghost">
                    Settings
                </Button>
            )}
        </div>,
        mod && st.settingsOpen && Settings(community, info),
        feed.length === 0 && (
            <p class="chat-hint">
                {info.may_post ? 'No posts yet. Write the first.' : 'No posts yet. Posts from members and from the fediverse appear here.'}
            </p>
        ),
        feed.map((p) => Card(community, p, pictureUrl, true)),
    ]
}

function Card(community: CommunityView, p: PostDto, pictureUrl: PictureUrl, tappable: boolean) {
    const mod = community.role === 'owner' || community.role === 'moderator'
    const url = pictureUrl(p.picture)
    const where = p.author.local ? 'member' : p.author.address
    const act = async (run: () => Promise<unknown>) => {
        try {
            await run()
            if (st.threadOpen === p.id) st.threadOpen = null
            await load(community.id)
        } catch (e) {
            failed(e)
        }
    }
    return (
        <article class="post-card" key={p.id}>
            <header>
                <b>{p.author.name}</b>
                <span class={p.author.local ? 'chat-sub' : 'chat-sub is-public'}>
                    {where}
                    {p.via && ` · via ${p.via}`} · {ago(p.created_ms)}
                </span>
            </header>
            <p class="post-text">{p.text}</p>
            {url && <img alt="" class="post-picture" src={url} />}
            <footer>
                {p.place ? (
                    <a href={placeLink(p.place)} rel="noopener" target="_blank">📍 {p.place.name ?? 'On the map'}</a>
                ) : (
                    <span class="chat-sub">No place: listed, not pinned</span>
                )}
                {tappable && p.parent == null && (
                    <button onclick={() => void openThread(p.id)} type="button">
                        {p.replies} {p.replies === 1 ? 'reply' : 'replies'}
                    </button>
                )}
                {p.mine && (
                    <button onclick={() => void act(() => signupApi.delete(`/api/posts/${p.id}`))} type="button">
                        Delete
                    </button>
                )}
                {mod && !p.mine && (
                    <button onclick={() => void act(() => signupApi.post(`/api/posts/${p.id}/hide`))} type="button">
                        Hide
                    </button>
                )}
                {mod && !p.author.local && (
                    <button
                        onclick={() =>
                            void act(() => signupApi.post(`/api/communities/${community.id}/blocks`, {address: p.author.address}))}
                        title="They can't post here again, and what they wrote here is hidden"
                        type="button"
                    >
                        Block {p.author.address}
                    </button>
                )}
            </footer>
        </article>
    )
}

function Thread(community: CommunityView, pictureUrl: PictureUrl) {
    const t = st.thread
    const back = (
        <button class="post-back" onclick={() => (st.threadOpen = null)} type="button">‹ Posts</button>
    )
    if (!t) return [back, <p class="chat-hint">Reading the thread…</p>]
    return [
        back,
        Card(community, t.post, pictureUrl, false),
        <h3 class="chat-pane-h">{t.replies.length} {t.replies.length === 1 ? 'reply' : 'replies'}</h3>,
        t.replies.map((r) => Card(community, r, pictureUrl, false)),
        <form
            class="post-form"
            onsubmit={async (e: Event) => {
                e.preventDefault()
                const text = st.reply.trim()
                if (!text || st.busy) return
                st.busy = true
                try {
                    const reply = await signupApi.post<PostDto>(`/api/posts/${t.post.id}/replies`, {text})
                    t.replies.push(reply)
                    t.post.replies += 1
                    st.reply = ''
                    st.busy = false
                } catch (err) {
                    failed(err)
                }
                m.redraw()
            }}
        >
            <FieldTextarea
                maxlength={500}
                model={{
                    get value() {
                        return st.reply
                    },
                    set value(v: string) {
                        st.reply = v
                    },
                }}
                placeholder="Reply in public"
                rows={2}
            />
            <Button disabled={!st.reply.trim() || st.busy} size="sm" type="submit">Reply</Button>
        </form>,
        <p class="chat-hint">Replies are public, like the post, and reach its author wherever they are.</p>,
    ]
}

function Composer(community: CommunityView, d: Draft, pictureUrl: PictureUrl) {
    const info = st.info.get(community.id)
    const url = pictureUrl(d.picture)
    const reach = info?.federating
        ? `Public. On the community's page and map, and sent to its ${info.followers} follower${info.followers === 1 ? '' : 's'} on other servers. Posted under your own address. Deleting it later asks other servers to delete their copy; they may not.`
        : "Public. On the community's page and map, for anybody on this server."
    const send = async () => {
        if (!d.place || !d.text.trim() || st.busy) return
        st.busy = true
        try {
            const post = await signupApi.post<PostDto>(`/api/communities/${community.id}/posts`, {
                picture: d.picture ?? undefined,
                place: d.place,
                text: d.text.trim(),
            })
            const feed = st.feed.get(community.id) ?? []
            st.feed.set(community.id, [post, ...feed])
            st.draft = null
            st.busy = false
        } catch (e) {
            failed(e)
        }
        m.redraw()
    }
    return (
        <div class="post-form">
            <h3 class="chat-pane-h">New post</h3>
            <FieldTextarea
                maxlength={500}
                model={{
                    get value() {
                        return d.text
                    },
                    set value(v: string) {
                        d.text = v
                    },
                }}
                placeholder="What's happening here?"
                rows={4}
            />
            {url && <img alt="" class="post-picture" src={url} />}
            <label class="m-button is-ghost is-sm">
                {d.picture ? 'Another picture' : 'Add a picture'}
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
            <div class="post-place">
                <b>📍 {d.place ? (d.place.name ?? `${d.place.latitude.toFixed(5)}, ${d.place.longitude.toFixed(5)}`) : 'Where is it?'}</b>
                <FieldText
                    model={{
                        get value() {
                            return d.where
                        },
                        set value(v: string) {
                            d.where = v
                            const p = parsePlace(v)
                            if (p) d.place = p
                        },
                    }}
                    placeholder="Paste a Cartopolis place link, or lat, lng"
                />
                <div class="post-actions">
                    {community.settings.place && (
                        <Button
                            onclick={() => {
                                const p = community.settings.place!
                                d.place = {latitude: p.latitude, longitude: p.longitude, name: p.name}
                            }}
                            size="sm"
                            variant="ghost"
                        >
                            The community's place
                        </Button>
                    )}
                    <Button
                        onclick={() =>
                            navigator.geolocation?.getCurrentPosition(
                                (pos) => {
                                    d.place = {latitude: pos.coords.latitude, longitude: pos.coords.longitude}
                                    m.redraw()
                                },
                                () => failed(new Error('this browser would not say where it is')),
                            )}
                        size="sm"
                        variant="ghost"
                    >
                        Where I am
                    </Button>
                </div>
                <p class="chat-hint">In the app, "Post this view" pins it where the camera is looking.</p>
            </div>
            <p class="post-reach is-box">{reach}</p>
            <div class="post-actions">
                <Button disabled={!d.place || !d.text.trim() || st.busy} onclick={() => void send()}>Post</Button>
                <Button onclick={() => (st.draft = null)} variant="ghost">Discard</Button>
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
                                ×
                            </button>
                        </div>
                    ))}
                    <form
                        class="chat-inline"
                        onsubmit={async (e: Event) => {
                            e.preventDefault()
                            const address = st.follow.trim()
                            if (!address.includes('@')) return
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
                            placeholder="groningen@feddit.nl or someone@mastodon.social"
                        />
                        <Button size="sm" type="submit">Follow</Button>
                    </form>
                </>
            )}
        </div>
    )
}
