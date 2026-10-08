import m, {MithrilComponent} from '@garage44/mithril'

import type {BuildingAddress, OwnedBuilding} from '@/types'

import {$s} from '@/app'
import {local} from '@/lib/reactive'
import {claimBuilding, releaseBuilding, searchBuildings} from '@/lib/signup'

/** Where the floor-plan editor for one building lives. */
export function editorPath(building: Pick<OwnedBuilding, 'latitude' | 'longitude' | 'pand_id'>) {
    return `/building/${building.pand_id}?lat=${building.latitude}&lng=${building.longitude}`
}

/**
 * The game, opened over a place: the `/go?place=` link the app and the web
 * client both answer (`systems::place_link`), looking down at it from 45 m.
 */
export function gamePlaceUrl(lat: number, lng: number) {
    return `/go?place=${lat.toFixed(5)},${lng.toFixed(5)},45m,-50,0`
}

/**
 * The buildings this account owns, and a way to claim another.
 *
 * The search is on a button and not on every keystroke. It reaches two public
 * registers that somebody else pays for, through a server that rate-limits it,
 * and "Grote Markt 1" typed at speed is fourteen queries for one answer. A
 * debounce would only make it four.
 */
export class BuildingPicker extends MithrilComponent {
    s = local({
        busy: false,
        claiming: '',
        confirming: '',
        query: '',
        results: null as BuildingAddress[] | null,
    })

    search = async (e: Event) => {
        e.preventDefault()
        const s = this.s
        if (s.busy || !s.query.trim()) {
            return
        }
        s.busy = true
        s.results = await searchBuildings(s.query)
        s.busy = false
    }

    take = async (address: BuildingAddress) => {
        const s = this.s
        s.claiming = address.nummeraanduiding_id
        const ok = await claimBuilding(address)
        s.claiming = ''
        if (ok) {
            s.results = null
            s.query = ''
        }
    }

    view() {
        const s = this.s
        const owned = $s.myBuildings
        const atLimit = $s.buildingLimit > 0 && owned.length >= $s.buildingLimit
        const results = s.results

        return (
            <>
                {owned.length > 0 && (
                    <div class="card">
                        <h2>Your buildings</h2>
                        <ul class="house-list">
                            {owned.map((building) => (
                                <li class="house" key={building.pand_id}>
                                    <span class="house-address">{building.address}</span>
                                    {s.confirming === building.pand_id ? (
                                        <>
                                            <button
                                                class="danger"
                                                onclick={() => {
                                                    s.confirming = ''
                                                    void releaseBuilding(building)
                                                }}
                                                type="button"
                                            >
                                                Give it up
                                            </button>
                                            <button
                                                class="ghost"
                                                onclick={() => {
                                                    s.confirming = ''
                                                }}
                                                type="button"
                                            >
                                                Keep
                                            </button>
                                        </>
                                    ) : (
                                        <>
                                            <button
                                                class="primary"
                                                onclick={() => m.route.set(editorPath(building))}
                                                type="button"
                                            >
                                                Design
                                            </button>
                                            <a
                                                class="ghost-link"
                                                href={gamePlaceUrl(building.latitude, building.longitude)}
                                                rel="noreferrer"
                                                target="_blank"
                                            >
                                                View in the game
                                            </a>
                                            <button
                                                class="ghost"
                                                onclick={() => {
                                                    s.confirming = building.pand_id
                                                }}
                                                type="button"
                                            >
                                                Give up
                                            </button>
                                        </>
                                    )}
                                </li>
                            ))}
                        </ul>
                        <p class="hint">
                            Design lays out the floors, walls, doors and stairs inside the building. What you
                            save is what everybody sees when they walk in — or tap the building in the game
                            and choose Look inside.
                        </p>
                    </div>
                )}

                <div class="card">
                    <h2>{owned.length > 0 ? 'Claim another building' : 'Claim a building'}</h2>
                    <p class="hint">
                        Groningen only, for now — that's where the map has buildings. One owner per building,
                        first come first served
                        {$s.buildingLimit > 0 ? `, up to ${$s.buildingLimit} per account` : ''}. Type any
                        address in it.
                    </p>
                    {atLimit ? (
                        <div class="empty">You own as many buildings as an account may.</div>
                    ) : (
                        <form class="house-search" onsubmit={this.search}>
                            <label class="field">
                                <span>An address in the building</span>
                                <input
                                    autocomplete="street-address"
                                    disabled={s.busy}
                                    oninput={(e: Event) => {
                                        s.query = (e.target as HTMLInputElement).value
                                    }}
                                    placeholder="Grote Markt 1"
                                    type="search"
                                    value={s.query}
                                />
                            </label>
                            <button class="primary" disabled={s.busy || !s.query.trim()} type="submit">
                                {s.busy ? 'Looking…' : 'Find it'}
                            </button>
                        </form>
                    )}

                    {results === null ? null : results.length === 0 ? (
                        <div class="empty">Nothing in Groningen matches that.</div>
                    ) : (
                        <ul class="house-list">
                            {results.map((address) => (
                                <li class="house" key={address.nummeraanduiding_id}>
                                    <span class="house-address">{address.address}</span>
                                    {address.available ? (
                                        <button
                                            class="primary"
                                            disabled={s.claiming !== ''}
                                            onclick={() => void this.take(address)}
                                            type="button"
                                        >
                                            {s.claiming === address.nummeraanduiding_id ? 'Claiming…' : 'Claim'}
                                        </button>
                                    ) : (
                                        <span class="tag disabled">owned</span>
                                    )}
                                </li>
                            ))}
                        </ul>
                    )}
                </div>
            </>
        )
    }
}
