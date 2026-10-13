import type {PostPlace} from '@/types'

/**
 * The two pure pieces of a community's public posts (`components/pages/chat/posts.tsx`),
 * apart so they can be tested without a page.
 */

/** A handle to suggest for a community's name, as the server's `suggest_handle` does. */
export function suggestHandle(name: string): string {
    const folded = name
        .normalize('NFKD')
        .replace(/[̀-ͯ]/g, '')
        .toLowerCase()
        .replace(/[^a-z0-9]+/g, '_')
        .replace(/^_+|_+$/g, '')
        .slice(0, 30)
        .replace(/_+$/, '')
    return folded || 'community'
}

/**
 * The place a Cartopolis link names (`…/go?place=53.21940,6.56650,120m,-12.5,45.0`),
 * or a bare "lat, lng". `null` when it names no place on Earth.
 */
export function parsePlace(input: string): PostPlace | null {
    const text = input.trim()
    let value = text
    const q = text.indexOf('?')
    if (q >= 0) {
        const params = new URLSearchParams(text.slice(q + 1).split('#')[0])
        value = params.get('place') ?? ''
    }
    const parts = value.split(',').map((p) => p.trim())
    const latitude = Number(parts[0])
    const longitude = Number(parts[1])
    if (parts.length < 2 || parts[0] === '' || parts[1] === '') return null
    if (!Number.isFinite(latitude) || !Number.isFinite(longitude)) return null
    if (Math.abs(latitude) > 90 || Math.abs(longitude) > 180) return null
    const place: PostPlace = {latitude, longitude}
    const altitude = Number((parts[2] ?? '').replace(/m$/, ''))
    const pitch = Number(parts[3])
    const yaw = Number(parts[4])
    if (parts.length >= 5 && [altitude, pitch, yaw].every(Number.isFinite)) {
        place.view = {altitude, pitch, yaw}
    }
    return place
}

/** The link that opens the map at a post's view, as the app's place links do. */
export function placeLink(p: PostPlace): string {
    const alt = Math.round(p.view?.altitude ?? 250)
    const look = p.view ? `,${p.view.pitch.toFixed(1)},${p.view.yaw.toFixed(1)}` : ''
    return `/go?place=${p.latitude.toFixed(5)},${p.longitude.toFixed(5)},${alt}m${look}`
}
