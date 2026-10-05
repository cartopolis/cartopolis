import {useState} from 'preact/hooks'

import type {HouseAddress} from '@/types'

import {$s} from '@/app'
import {claimHouse, releaseHouse, searchHouses} from '@/lib/signup'

/**
 * Pick a house.
 *
 * The search is on a button and not on every keystroke. It reaches two public
 * registers that somebody else pays for, through a server that rate-limits it,
 * and "Grote Markt 1" typed at speed is fourteen queries for one answer. A
 * debounce would only make it four.
 */
export const HousePicker = () => {
    const [query, setQuery] = useState('')
    const [results, setResults] = useState<HouseAddress[] | null>(null)
    const [busy, setBusy] = useState(false)
    const [claiming, setClaiming] = useState('')

    const held = $s.myHouse

    const search = async (e: Event) => {
        e.preventDefault()
        if (busy || !query.trim()) {
            return
        }
        setBusy(true)
        setResults(await searchHouses(query))
        setBusy(false)
    }

    const take = async (address: HouseAddress) => {
        setClaiming(address.nummeraanduiding_id)
        const ok = await claimHouse(address)
        setClaiming('')
        if (ok) {
            setResults(null)
            setQuery('')
        }
    }

    if (held) {
        return (
            <div class="card">
                <h2>Your house</h2>
                <p class="house-held">{held.address}</p>
                <p class="hint">
                    It's yours in the register. The world doesn't draw it yet — that's still being
                    built — but the claim is recorded against the building itself, so it will be this
                    one.
                </p>
                <div class="row-actions">
                    <button class="ghost" onClick={() => void releaseHouse()} type="button">
                        Give it up
                    </button>
                </div>
            </div>
        )
    }

    return (
        <div class="card">
            <h2>Claim your house</h2>
            <p class="hint">
                Groningen only, for now — that's where the map has buildings. One house per account,
                first come first served.
            </p>
            <form class="house-search" onSubmit={search}>
                <label class="field">
                    <span>Your address</span>
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
                                <span class="tag disabled">taken</span>
                            )}
                        </li>
                    ))}
                </ul>
            )}
        </div>
    )
}
