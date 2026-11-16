import mServer from '@garage44/mithril/server'

import {PageView} from './frame'
import type {PageData} from './tree'

/**
 * The content pages' server renderer: built on its own as `dist/pages-ssr.js`
 * (`build.ts`), which the simulator's render process loads
 * (`tools/script-host/src/render.ts`) and asks for every visit to a live page.
 * It draws what `pages.tsx` draws for the same data, so the browser can take the
 * HTML over rather than draw it again (`frame.tsx` says what that rests on).
 *
 * The body's HTML, without the shell around it: the simulator puts it into
 * `pages.html`, beside the data it was drawn from.
 */
export async function renderPage(data: PageData): Promise<string> {
    const view = {view: () => PageView(data, {lang: data.lang, slug: data.slug}, false)}
    return (await mServer.renderToString(mServer(view))).html
}
