import m, {MithrilComponent} from '@garage44/mithril'
import {Badge, Button, ButtonGroup, Card, FieldText} from '@garage44/mithril/components'

import type {BuildingAddress, OwnedBuilding} from '@/types'

import {$s} from '@/app'
import {bind, local} from '@/lib/reactive'
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
 * A link out to the game, drawn as a library button. `Button` takes an `href`
 * but no `target`, and the game has to open beside this page rather than over a
 * floor plan that may hold unsaved work — so this is the `<a>` with the
 * button's classes, not the component.
 */
export class GameLink extends MithrilComponent<{lat: number; lng: number}> {
    view({attrs}: m.Vnode<{lat: number; lng: number}>) {
        return (
            <a
                class="m-button"
                data-size="sm"
                data-variant="ghost"
                href={gamePlaceUrl(attrs.lat, attrs.lng)}
                rel="noreferrer"
                target="_blank"
            >
                View in the game
            </a>
        )
    }
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
            <div class="building-picker">
                {owned.length > 0 && (
                    <Card
                        footer={
                            <p class="building-picker__hint">
                                Design lays out the floors, walls, doors and stairs inside the building. What you
                                save is what everybody sees when they walk in — or tap the building in the game
                                and choose Look inside.
                            </p>
                        }
                        flush
                        title="Your buildings"
                    >
                        <ul class="building-picker__list">
                            {owned.map((building) => (
                                <li class="building-picker__item" key={building.pand_id}>
                                    <span class="building-picker__address">{building.address}</span>
                                    {/*
                                      * Giving a building up is two presses: the first only
                                      * swaps the row's actions for a confirmation, because
                                      * a released building is anybody's to claim at once.
                                      */}
                                    {s.confirming === building.pand_id ? (
                                        <ButtonGroup>
                                            <Button
                                                onclick={() => {
                                                    s.confirming = ''
                                                    void releaseBuilding(building)
                                                }}
                                                size="sm"
                                                variant="danger"
                                            >
                                                Give it up
                                            </Button>
                                            <Button
                                                onclick={() => {
                                                    s.confirming = ''
                                                }}
                                                size="sm"
                                                variant="ghost"
                                            >
                                                Keep
                                            </Button>
                                        </ButtonGroup>
                                    ) : (
                                        <ButtonGroup>
                                            <Button
                                                onclick={() => m.route.set(editorPath(building))}
                                                size="sm"
                                                variant="primary"
                                            >
                                                Design
                                            </Button>
                                            <GameLink lat={building.latitude} lng={building.longitude} />
                                            <Button
                                                onclick={() => {
                                                    s.confirming = building.pand_id
                                                }}
                                                size="sm"
                                                variant="ghost"
                                            >
                                                Give up
                                            </Button>
                                        </ButtonGroup>
                                    )}
                                </li>
                            ))}
                        </ul>
                    </Card>
                )}

                <Card title={owned.length > 0 ? 'Claim another building' : 'Claim a building'}>
                    <p class="building-picker__hint">
                        Groningen only, for now — that's where the map has buildings. One owner per building,
                        first come first served
                        {$s.buildingLimit > 0 ? `, up to ${$s.buildingLimit} per account` : ''}. Type any
                        address in it.
                    </p>
                    {atLimit ? (
                        <p class="building-picker__empty">You own as many buildings as an account may.</p>
                    ) : (
                        <form class="building-picker__search" onsubmit={this.search}>
                            <FieldText
                                autocomplete="street-address"
                                disabled={s.busy}
                                label="An address in the building"
                                model={bind(s, 'query')}
                                placeholder="Grote Markt 1"
                                type="search"
                            />
                            <Button disabled={!s.query.trim()} loading={s.busy} type="submit" variant="primary">
                                Find it
                            </Button>
                        </form>
                    )}

                    {results === null ? null : results.length === 0 ? (
                        <p class="building-picker__empty">Nothing in Groningen matches that.</p>
                    ) : (
                        <ul class="building-picker__list">
                            {results.map((address) => (
                                <li class="building-picker__item" key={address.nummeraanduiding_id}>
                                    <span class="building-picker__address">{address.address}</span>
                                    {address.available ? (
                                        <Button
                                            disabled={s.claiming !== ''}
                                            loading={s.claiming === address.nummeraanduiding_id}
                                            onclick={() => void this.take(address)}
                                            size="sm"
                                            variant="primary"
                                        >
                                            Claim
                                        </Button>
                                    ) : (
                                        <Badge>owned</Badge>
                                    )}
                                </li>
                            ))}
                        </ul>
                    )}
                </Card>
            </div>
        )
    }
}
