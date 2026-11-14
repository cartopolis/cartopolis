import m, {MithrilComponent} from '@garage44/mithril'
import {Badge, Button, Table} from '@garage44/mithril/components'

import type {PageSummary} from '@/types'

import {api, notifier} from '@/app'
import {ApiError} from '@/lib/api'
import {local} from '@/lib/reactive'
import {may} from '@/lib/roles'

/**
 * Admin › Content › Pages (`docs/mockups/cms`): the site's content pages, what
 * is live, and where each language stands against the one it was written in.
 *
 * Writing is MCP's (`/mcp`, `crates/simulator/src/pages/mcp.rs`): an agent — the
 * Midas hub's — makes the JSX and the texts. This page is for looking and for the
 * decisions a person may want to take back: preview a draft, publish it, take a
 * page down.
 */
export class ContentPages extends MithrilComponent {
    s = local({busy: '', languages: [] as string[], loaded: false, pages: [] as PageSummary[]})

    oninit() {
        void this.load()
    }

    async load() {
        try {
            const r = await api.get<{pages: PageSummary[]; languages: string[]}>('/api/admin/pages')
            this.s.pages = r.pages
            this.s.languages = r.languages
        } catch (e) {
            notifier.error(e instanceof ApiError ? e.message : 'Could not load the pages')
        }
        this.s.loaded = true
    }

    async act(page: PageSummary, what: 'publish' | 'unpublish') {
        this.s.busy = page.slug
        try {
            await api.post(`/api/admin/pages/${encodeURIComponent(page.slug)}/${what}`)
            notifier.success(what === 'publish' ? `${page.title || page.slug} is live` : `${page.title || page.slug} is taken down`)
            await this.load()
        } catch (e) {
            notifier.error(e instanceof ApiError ? e.message : `Could not ${what} the page`)
        }
        this.s.busy = ''
    }

    view() {
        const s = this.s
        const publisher = may('pages_publish')

        const state = (p: PageSummary) => (
            <span class="content-pages-tags">
                {p.live_revision !== null ? (
                    <Badge dot tone="success">
                        live · revision {p.live_revision}
                    </Badge>
                ) : null}
                {p.draft_changed ? <Badge tone="warning">draft</Badge> : null}
            </span>
        )

        const language = (lang: string) => (p: PageSummary) => {
            const status = p.languages[lang]
            if (lang === p.source_lang) return <Badge tone="accent">source</Badge>
            if (!status || (!status.missing.length && !status.stale.length)) return <Badge tone="success">complete</Badge>
            return (
                <span class="content-pages-tags" title={[...status.missing.map((k) => `missing ${k}`), ...status.stale.map((k) => `stale ${k}`)].join('\n')}>
                    {status.missing.length ? <Badge>missing {status.missing.length}</Badge> : null}
                    {status.stale.length ? <Badge tone="warning">stale {status.stale.length}</Badge> : null}
                </span>
            )
        }

        const actions = (p: PageSummary) => (
            <span class="content-pages-tags">
                {s.languages.map((lang) => (
                    <a class="content-pages-preview" href={`/${lang}/${p.slug}?draft`} rel="noopener" target="_blank">
                        Preview {lang.toUpperCase()}
                    </a>
                ))}
                {publisher && (p.draft_changed || p.live_revision === null) ? (
                    <Button disabled={s.busy === p.slug} onclick={() => void this.act(p, 'publish')} size="sm" variant="primary">
                        Publish
                    </Button>
                ) : null}
                {publisher && p.live_revision !== null ? (
                    <Button disabled={s.busy === p.slug} onclick={() => void this.act(p, 'unpublish')} size="sm" variant="ghost">
                        Take down
                    </Button>
                ) : null}
            </span>
        )

        return (
            <div class="content-pages">
                <p class="content-pages-intro">
                    Pages are written through MCP at <code>/mcp</code> with an access token that may write pages. Each text is a
                    $t key with a text per language; a translation turns stale when its source text changes.
                </p>
                <Table
                    columns={[
                        {
                            cell: (p: PageSummary) => (
                                <span class="content-pages-title">
                                    {p.title || p.slug}
                                    <small>
                                        /{p.slug} · {p.updated_by} · {new Date(p.updated_at * 1000).toLocaleString()}
                                    </small>
                                </span>
                            ),
                            key: 'title',
                            label: 'Page',
                        },
                        {cell: state, key: 'state', label: 'State'},
                        ...s.languages.map((lang) => ({cell: language(lang), key: `lang-${lang}`, label: lang.toUpperCase()})),
                        {align: 'end' as const, cell: actions, key: 'actions', label: ''},
                    ]}
                    empty={s.loaded ? 'No pages yet.' : 'Loading…'}
                    rowKey={(p: PageSummary) => p.slug}
                    rows={s.pages}
                />
            </div>
        )
    }
}
