import m, {MithrilComponent} from '@garage44/mithril'
import {Alert, Button} from '@garage44/mithril/components'

import type {CommunityLink} from '@/types'

import {$s} from '@/app'
import {ProviderLogo} from '@/components/elements/providers/providers'
import {begin, loadCommunityLinks, unlinkGroup} from '@/lib/auth'
import {local} from '@/lib/reactive'
import {formatUnix} from '@/lib/tokens'

interface LinkedGroupsAttrs {
    id: string
}

/**
 * A community's linked Discord servers and subreddits, in its settings
 * (`docs/mockups/external-sign-in/v3-communities.html`, section 4): whoever
 * signs in through one is offered to join this community.
 *
 * For owners and moderators only, which the caller decides; the server checks
 * again. Linking is a round trip to the provider, because only the provider can
 * say which servers this person manages or which subreddits they moderate — so
 * the buttons leave the page, and `/welcome` brings the person back here.
 */
export class LinkedGroups extends MithrilComponent<LinkedGroupsAttrs> {
    s = local({busy: false, links: null as CommunityLink[] | null})

    load = async (id: string) => {
        this.s.links = (await loadCommunityLinks(id)) ?? []
    }

    oninit(vnode: m.Vnode<LinkedGroupsAttrs>) {
        void this.load(vnode.attrs.id)
    }

    view(vnode: m.Vnode<LinkedGroupsAttrs>) {
        const s = this.s
        const id = vnode.attrs.id
        // Only the providers the server has switched on can be linked.
        const linkable = ($s.registration?.providers ?? []).filter((p) => p.kind === 'discord' || p.kind === 'reddit')
        const links = s.links
        if (!linkable.length && !links?.length) return null
        return (
            <div class="linked-groups">
                <h3 class="chat-pane-h">Linked groups</h3>
                <p class="chat-hint">Members of these can join this community straight from sign-in.</p>
                {links === null && <p class="chat-hint">Loading…</p>}
                {links?.map((link) => (
                    <div class="linked-group" key={`${link.provider}/${link.group_id}`}>
                        {ProviderLogo({id: link.provider}, 20)}
                        <div class="linked-group__text">
                            <b>{link.group_name}</b>
                            <small>
                                {link.provider === 'reddit' ? 'Subreddit' : `${link.provider_name} server`} · linked by{' '}
                                {link.linked_by} · {formatUnix(link.linked_at, '')}
                            </small>
                        </div>
                        <Button
                            disabled={s.busy}
                            onclick={async () => {
                                s.busy = true
                                if (await unlinkGroup(id, link)) await this.load(id)
                                s.busy = false
                            }}
                            size="sm"
                            variant="danger"
                        >
                            Unlink
                        </Button>
                    </div>
                ))}
                {linkable.length > 0 && (
                    <div class="linked-groups__add">
                        {linkable.map((p) => (
                            <Button
                                disabled={s.busy}
                                icon={ProviderLogo(p)}
                                key={p.id}
                                onclick={async () => {
                                    s.busy = true
                                    if (!(await begin(p.id, 'community', {community: id, return: `/?show=${encodeURIComponent(id)}`}))) {
                                        s.busy = false
                                    }
                                }}
                                size="sm"
                            >
                                {p.kind === 'reddit' ? 'Link a subreddit' : `Link a ${p.name} server`}
                            </Button>
                        ))}
                    </div>
                )}
                {linkable.length > 0 && (
                    <Alert tone="info">
                        You can only link a Discord server you manage, or a subreddit you moderate.
                    </Alert>
                )}
            </div>
        )
    }
}
