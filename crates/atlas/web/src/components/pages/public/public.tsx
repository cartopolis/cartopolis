import m, {MithrilComponent} from '@garage44/mithril'

import {$s, signupApi} from '@/app'
import {Card, FollowFrom, load, readOf, type PostsCtx} from '@/components/pages/chat/posts'
import {ApiError} from '@/lib/api'
import type {PersonPosts, PublicInfo} from '@/types'

/**
 * The public pages: a community's posts (`/c/{handle}`) and a person's
 * (`/@{name}`), for anybody, signed in or not (`docs/notes/fediverse.md`).
 *
 * They are where other servers send people. A community's actor and a
 * person's name `url` these pages, so "Open original" on Mastodon and the
 * profile link on Lemmy land on what was written here, with a box to follow it
 * from the reader's own server, rather than on the site's front door. The
 * cards are the community page's own (`chat/posts.tsx`), drawn with what a
 * reader with no session can see: pictures from `/ap/media/`, the public copy
 * every public picture has, and faces as initials.
 */
const ctx: PostsCtx = {
    avatar: (address: string, size: number) => (
        <span class="post-page-face" style={`width:${size}px;height:${size}px;font-size:${Math.round(size * 0.42)}px`}>
            {address.slice(0, 1).toUpperCase()}
        </span>
    ),
    // A post's place opens the viewer there, as a place link does.
    onMap: (place: string) => location.assign(`/go?place=${place}`),
    pictureUrl: (hash) => (hash ? `/ap/media/${hash}` : null),
}

function said(e: unknown): string {
    if (e instanceof ApiError && e.status === 404) return 'There is nothing public here.'
    return e instanceof Error ? e.message : String(e)
}

/** A community's public half, by its handle. */
export class PublicCommunity extends MithrilComponent<{handle: string}> {
    id: string | null = null
    error = ''
    asked = ''

    async read(handle: string) {
        this.asked = handle
        this.id = null
        this.error = ''
        try {
            const info = await signupApi.get<PublicInfo>(`/api/communities/${encodeURIComponent(handle)}/public`)
            if (!info.community) throw new ApiError(404, 'not public')
            this.id = info.community.id
            await load(this.id)
        } catch (e) {
            this.error = said(e)
        }
        m.redraw()
    }

    view(vnode: m.Vnode<{handle: string}>) {
        const handle = vnode.attrs.handle
        if (this.asked !== handle) void this.read(handle)
        if (this.error) return <div class="post-page"><p class="post-empty">{this.error}</p></div>
        const {info, feed} = this.id ? readOf(this.id) : {feed: [], info: undefined}
        const c = info?.community
        if (!info || !c || !this.id) return <div class="post-page"><p class="post-dim">Reading…</p></div>
        const where = {id: this.id, mod: info.may_moderate, signedIn: !!$s.signup.token}
        return (
            <div class="post-page">
                <div class="post-page-hero">
                    {c.picture ? <img alt="" src={`/ap/media/${c.picture}`} /> : <span class="post-page-face">{c.name.slice(0, 1)}</span>}
                    <div>
                        <h1>{c.name}</h1>
                        <span class="post-public">
                            @{info.address} · {info.followers} follower{info.followers === 1 ? '' : 's'} on the fediverse
                        </span>
                    </div>
                </div>
                {c.about && <p class="post-page-about">{c.about}</p>}
                {info.address && info.federating && FollowFrom(info.address, true)}
                {feed.length === 0 && <p class="post-empty">Nothing posted yet.</p>}
                {feed.map((p) => Card(where, p, ctx, true))}
            </div>
        )
    }
}

/** A person's public posts, by their name. */
export class PublicPerson extends MithrilComponent<{name: string}> {
    page: PersonPosts | null = null
    error = ''
    asked = ''

    async read(name: string) {
        this.asked = name
        this.page = null
        this.error = ''
        try {
            this.page = await signupApi.get<PersonPosts>(`/api/posts?author=${encodeURIComponent(name)}`)
        } catch (e) {
            this.error = said(e)
        }
        m.redraw()
    }

    view(vnode: m.Vnode<{name: string}>) {
        const name = decodeURIComponent(vnode.attrs.name)
        if (this.asked !== name) void this.read(name)
        if (this.error) return <div class="post-page"><p class="post-empty">{this.error}</p></div>
        const page = this.page
        if (!page) return <div class="post-page"><p class="post-dim">Reading…</p></div>
        const signedIn = !!$s.signup.token
        return (
            <div class="post-page">
                <div class="post-page-hero">
                    {page.picture ? <img alt="" src={`/ap/media/${page.picture}`} /> : <span class="post-page-face">{name.slice(0, 1).toUpperCase()}</span>}
                    <div>
                        <h1>{page.address.split('@')[0]}</h1>
                        <span class="post-public">@{page.address}</span>
                    </div>
                </div>
                {FollowFrom(page.address, false)}
                {page.posts.length === 0 && <p class="post-empty">No public posts yet.</p>}
                {page.posts.map((p) => Card({id: p.community, mod: false, signedIn}, p, ctx, true))}
            </div>
        )
    }
}
