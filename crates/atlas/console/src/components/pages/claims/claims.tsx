import {useEffect, useState} from 'preact/hooks'

import {$s} from '@/app'
import {loadClaims, releaseClaim} from '@/lib/accounts'
import {formatTimestamp} from '@/lib/utils'

/**
 * Who holds which house.
 *
 * This list *is* the dispute mechanism. Claiming is first come first served with
 * no proof of residence, so the only answer to "that is my house" is an operator
 * with this page and the button on the right of the row — which is why the row
 * shows the holder's name and the date, and not just the address.
 */
export const Claims = () => {
    const [confirming, setConfirming] = useState('')

    useEffect(() => {
        void loadClaims()
    }, [])

    const claims = $s.claims

    return (
        <>
            <div class="section-head">
                <h1>Houses</h1>
                <span class="count">{claims.length}</span>
            </div>

            {claims.length === 0 ? (
                <div class="empty">Nobody has claimed a house yet.</div>
            ) : (
                <ul class="account-list">
                    {claims.map((claim) => (
                        <li class="account" key={claim.nummeraanduiding_id}>
                            <div class="account-main">
                                <span class="account-name">{claim.address}</span>
                                <span style="flex:1" />
                                {/*
                                  * Two presses, like the account delete: releasing
                                  * a house takes it off somebody who believes it
                                  * is theirs, and there is no undo but asking them
                                  * to claim it again first.
                                  */}
                                {confirming === claim.nummeraanduiding_id ? (
                                    <>
                                        <button
                                            class="danger"
                                            onClick={() => {
                                                setConfirming('')
                                                void releaseClaim(claim)
                                            }}
                                            type="button"
                                        >
                                            Release
                                        </button>
                                        <button
                                            class="ghost"
                                            onClick={() => setConfirming('')}
                                            type="button"
                                        >
                                            Cancel
                                        </button>
                                    </>
                                ) : (
                                    <button
                                        class="ghost"
                                        onClick={() => setConfirming(claim.nummeraanduiding_id)}
                                        type="button"
                                    >
                                        Release
                                    </button>
                                )}
                            </div>
                            <div class="account-meta">
                                <span>{claim.username || '[deleted]'}</span>
                                <span>{formatTimestamp(claim.claimed_at)}</span>
                                {/* The join key to the geometry, and the thing to
                                    quote when a claim has to be traced to a
                                    building. */}
                                <span class="mono">{claim.pand_id}</span>
                            </div>
                        </li>
                    ))}
                </ul>
            )}
        </>
    )
}
