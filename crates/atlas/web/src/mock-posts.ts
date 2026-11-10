/**
 * The posts half of a community and Activity, for `--mock` (`mock.ts`), so the
 * public posts, their buttons and the notices list can be looked at with no
 * simulator. Invented content in the shapes `cartopolis_core::posts` sends.
 */
import type {PostAuthor, PostDto, PostNotices, PostThread, PublicInfo} from '@/types'

const HOUR = 3_600_000
const now = Date.now()

const me: PostAuthor = {address: 'annekedijkstra@localhost', local: true, name: 'annekedijkstra'}
const sanne: PostAuthor = {address: 'sanne@localhost', local: true, name: 'sanne'}
const anna: PostAuthor = {address: 'anna@mastodon.social', local: false, name: 'Anna de Vries'}
const piet: PostAuthor = {address: 'piet@feddit.nl', local: false, name: 'Piet'}

function post(id: string, community: string, author: PostAuthor, text: string, hoursAgo: number, extra: Partial<PostDto> = {}): PostDto {
    return {
        author,
        boosted: false,
        boosts: 0,
        community,
        created_ms: now - hoursAgo * HOUR,
        dislikes: 0,
        id,
        liked: false,
        likes: 0,
        mine: author === me,
        replies: 0,
        sensitive: false,
        text,
        ...extra,
    }
}

const feeds = new Map<string, PostDto[]>()
const threads = new Map<string, PostDto[]>()

function seed(community: string): PostDto[] {
    const have = feeds.get(community)
    if (have) return have
    const feed = [
        post(`${community}-1`, community, sanne, 'Plant swap on the corner this Saturday, 10 to 1. Bring cuttings, take cuttings.', 2, {
            boosts: 2,
            likes: 5,
            liked: true,
            place: {latitude: 53.2129, longitude: 6.5823, name: 'Oosterpark'},
            replies: 2,
        }),
        post(`${community}-2`, community, anna, 'The bridge works start Monday; the route below is the detour for bikes.', 5, {
            likes: 3,
            via: 'groningen@feddit.nl',
            warning: 'Road works, long post',
        }),
        post(`${community}-3`, community, me, 'New benches with planters by the playground!', 26, {
            boosts: 1,
            likes: 4,
            place: {latitude: 53.2141, longitude: 6.5852, name: 'Speeltuin Oosterpark'},
        }),
    ]
    feeds.set(community, feed)
    threads.set(`${community}-1`, [
        post(`${community}-1a`, community, anna, 'I will bring tomato seedlings!', 1.5, {likes: 1}),
        post(`${community}-1b`, community, piet, '@anna@mastodon.social got any left for me?', 1, {answers: 'Anna de Vries', parent: `${community}-1`}),
    ])
    return feed
}

function find(id: string): PostDto | undefined {
    for (const list of [...feeds.values(), ...threads.values()]) {
        const p = list.find((x) => x.id === id)
        if (p) return p
    }
    return undefined
}

const notices: PostNotices = {
    items: [
        {community: 'oost', created_ms: now - 0.3 * HOUR, excerpt: 'New benches with planters by the playground!', id: 4, kind: 'like', post: 'oost-3', read: false, who: anna},
        {community: 'oost', created_ms: now - 1 * HOUR, excerpt: '@anna@mastodon.social got any left for me?', id: 3, kind: 'reply', post: 'oost-1b', read: false, who: piet},
        {community: 'oost', created_ms: now - 3 * HOUR, excerpt: 'New benches with planters by the playground!', id: 2, kind: 'boost', post: 'oost-3', read: true, who: sanne},
        {created_ms: now - 30 * HOUR, id: 1, kind: 'follow', read: true, who: anna},
    ],
    unread: 2,
}

function json(body: unknown, status = 200): Response {
    return new Response(JSON.stringify(body), {headers: {'content-type': 'application/json'}, status})
}

/** Answer a posts request, or `null` when it is not one. */
export async function mockPosts(req: Request, p: string): Promise<Response | null> {
    const m = req.method
    let hit = p.match(/^\/api\/communities\/([^/]+)\/public$/)
    if (hit && m === 'GET') {
        const info: PublicInfo = {
            address: `${hit[1]}@localhost`,
            federating: true,
            followers: 12,
            follows: [{accepted: true, address: 'groningen@feddit.nl', kind: 'community', name: 'Groningen'}],
            may_moderate: true,
            may_post: true,
            open_reports: 0,
            settings: {enabled: true, handle: hit[1], who_posts: 'members'},
        }
        return json(info)
    }
    hit = p.match(/^\/api\/communities\/([^/]+)\/posts$/)
    if (hit && m === 'GET') return json(seed(hit[1]))
    if (hit && m === 'POST') {
        const b = (await req.json()) as {text: string; warning?: string}
        const made = post(`new-${Date.now()}`, hit[1], me, b.text, 0, {warning: b.warning})
        seed(hit[1]).unshift(made)
        return json(made, 201)
    }
    if (p === '/api/fediverse/notices' && m === 'GET') return json(notices)
    if (p === '/api/fediverse/notices/read' && m === 'POST') {
        notices.unread = 0
        notices.items.forEach((n) => (n.read = true))
        return new Response(null, {status: 204})
    }
    hit = p.match(/^\/api\/posts\/([^/]+)\/(like|boost)$/)
    if (hit && m === 'POST') {
        const target = find(hit[1])
        if (!target) return json({error: 'no such post'}, 404)
        const {on} = (await req.json()) as {on: boolean}
        if (hit[2] === 'like' && on !== target.liked) target.likes += on ? 1 : -1
        if (hit[2] === 'boost' && on !== target.boosted) target.boosts += on ? 1 : -1
        if (hit[2] === 'like') target.liked = on
        else target.boosted = on
        return json(target)
    }
    hit = p.match(/^\/api\/posts\/([^/]+)\/report$/)
    if (hit && m === 'POST') return new Response(null, {status: 204})
    hit = p.match(/^\/api\/posts\/([^/]+)\/replies$/)
    if (hit && m === 'POST') {
        const to = find(hit[1])
        if (!to) return json({error: 'no such post'}, 404)
        const root = to.parent ?? to.id
        const {text} = (await req.json()) as {text: string}
        const reply = post(`r-${Date.now()}`, to.community, me, text, 0, {answers: to.author.name, parent: root})
        threads.set(root, [...(threads.get(root) ?? []), reply])
        return json(reply, 201)
    }
    hit = p.match(/^\/api\/posts\/([^/]+)$/)
    if (hit && m === 'GET') {
        const top = find(hit[1])
        if (!top) return json({error: 'no such post'}, 404)
        const thread: PostThread = {post: top, replies: threads.get(top.id) ?? []}
        return json(thread)
    }
    return null
}
