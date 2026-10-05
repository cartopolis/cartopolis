import {route} from 'preact-router'
import {useState} from 'preact/hooks'

import type {BuildingAddress, OwnedBuilding} from '@/types'

import {$s} from '@/app'
import {claimBuilding, releaseBuilding, searchBuildings} from '@/lib/signup'

/** Where the floor-plan editor for one building lives. */
export function editorPath(building: Pick<OwnedBuilding, 'latitude' | 'longitude' | 'pand_id'>) {
    return `/building/${building.pand_id}?lat=${building.latitude}&lng=${building.longitude}`
}

/**
 * The buildings this account owns, and a way to claim another.
 *
 * The search is on a button and not on every keystroke. It reaches two public
 * registers that somebody else pays for, through a server that rate-limits it,
 * and "Grote Markt 1" typed at speed is fourteen queries for one answer. A
 * debounce would only make it four.
 */
export const BuildingPicker = () => {
    const [query, setQuery] = useState('')
    const [results, setResults] = useState<BuildingAddress[] | null>(null)
    const [busy, setBusy] = useState(false)
    const [claiming, setClaiming] = useState('')
    const [confirming, setConfirming] = useState('')

    const owned = $s.myBuildings
    const atLimit = $s.buildingLimit > 0 && owned.length >= $s.buildingLimit

    const search = async (e: Event) => {
        e.preventDefault()
        if (busy || !query.trim()) {
            return
        }
        setBusy(true)
        setResults(await searchBuildings(query))
        setBusy(false)
    }

    const take = async (address: BuildingAddress) => {
        setClaiming(address.nummeraanduiding_id)
        const ok = await claimBuilding(address)
        setClaiming('')
        if (ok) {
            setResults(null)
            setQuery('')
        }
    }

    return (
        <>
            {owned.length > 0 && (
                <div class="card">
                    <h2>Your buildings</h2>
                    <ul class="house-list">
                        {owned.map((building) => (
                            <li class="house" key={building.pand_id}>
                                <span class="house-address">{building.address}</span>
                                {confirming === building.pand_id ? (
                                    <>
                                        <button
                                            class="danger"
                                            onClick={() => {
                                                setConfirming('')
                                                void releaseBuilding(building)
                                            }}
                                            type="button"
                                        >
                                            Give it up
                                        </button>
                                        <button class="ghost" onClick={() => setConfirming('')} type="button">
                                            Keep
                                        </button>
                                    </>
                                ) : (
                                    <>
                                        <button
                                            class="primary"
                                            onClick={() => route(editorPath(building))}
                                            type="button"
                                        >
                                            Design
                                        </button>
                                        <button
                                            class="ghost"
                                            onClick={() => setConfirming(building.pand_id)}
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
                        save is what everybody sees when they walk in.
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
                    <form class="house-search" onSubmit={search}>
                        <label class="field">
                            <span>An address in the building</span>
                            <input
                                autocomplete="street-address"
                                disabled={busy}
                                onInput={(e) => setQuery((e.target as HTMLInputElement).value)}
                                placeholder="Grote Markt 1"
                                type="search"
                                value={query}
                            />
                        </label>
                        <button class="primary" disabled={busy || !query.trim()} type="submit">
                            {busy ? 'Looking…' : 'Find it'}
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
                                        disabled={claiming !== ''}
                                        onClick={() => void take(address)}
                                        type="button"
                                    >
                                        {claiming === address.nummeraanduiding_id ? 'Claiming…' : 'Claim'}
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
