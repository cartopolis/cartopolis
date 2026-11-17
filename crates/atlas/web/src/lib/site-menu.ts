import m from '@garage44/mithril'

import {LANG_KEY} from '@/cms/sitebar'
import {NO_MENU, type SiteMenu} from '@/cms/tree'

/**
 * The site's menu as the app's bar draws it (`cms/sitebar.tsx`,
 * `docs/mockups/site-header` v2): the content pages a page has put in the
 * menu, in the language the reader last read the site in.
 *
 * The app's own words are English; the bar's are the site's, so they follow
 * that language. It is the one the content pages store when read (`LANG_KEY`),
 * else the browser's, and the bar's switch changes it.
 */

/** `GET /api/pages?lang=`. */
interface MenuAnswer extends SiteMenu {
    languages: string[]
}

export const site = {
    lang: initialLang(),
    languages: [] as string[],
    menu: NO_MENU as SiteMenu,
}

function initialLang(): string {
    try {
        const kept = localStorage.getItem(LANG_KEY)
        if (kept) return kept
    } catch {
        // No storage: the browser's language.
    }
    return (navigator.language || 'en').slice(0, 2).toLowerCase()
}

let asked = ''

/** Ask for the menu in the current language, once per language. */
export async function loadSiteMenu(): Promise<void> {
    const lang = site.lang
    if (asked === lang) return
    asked = lang
    try {
        const res = await fetch(`/api/pages?lang=${encodeURIComponent(lang)}`)
        if (!res.ok) return
        const answer = (await res.json()) as MenuAnswer
        if (site.lang !== lang) return
        site.languages = answer.languages
        site.menu = {home: answer.home, pages: answer.pages}
        // A language the server does not serve: the bar speaks its first.
        if (answer.languages.length && !answer.languages.includes(lang)) {
            site.lang = answer.languages[0]
            asked = site.lang
        }
        m.redraw()
    } catch {
        // No menu: the bar draws the app's places and the mark, which is enough.
        asked = ''
    }
}

export function setSiteLang(lang: string) {
    site.lang = lang
    try {
        localStorage.setItem(LANG_KEY, lang)
    } catch {
        // Kept for this page only.
    }
    void loadSiteMenu()
}
