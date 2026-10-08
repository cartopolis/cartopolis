import m, {MithrilComponent} from '@garage44/mithril'

import {$s} from '@/app'
import {loadClaims, releaseClaim} from '@/lib/accounts'
import {local} from '@/lib/reactive'
import {formatTimestamp} from '@/lib/utils'

/**
 * Who owns which building.
 *
 * This list *is* the dispute mechanism. Claiming is first come first served with
 * no proof of residence, so the only answer to "that is my house" is an operator
 * with this page and the button on the right of the row — which is why the row
 * shows the holder's name and the date, and not just the address.
 */
export class Claims extends MithrilComponent {
    s = local({confirming: ''})

    oninit() {
        void loadClaims()
    }

    view() {
        const s = this.s
        const claims = $s.claims

        return (
            <>
                <div class="section-head">
                    <h1>Buildings</h1>
                    <span class="count">{claims.length}</span>
                </div>

                {claims.length === 0 ? (
                    <div class="empty">Nobody has claimed a building yet.</div>
                ) : (
                    <ul class="account-list">
                        {claims.map((claim) => (
                            <li class="account" key={claim.pand_id}>
                                <div class="account-main">
                                    <span class="account-name">{claim.address}</span>
                                    <span style="flex:1" />
                                    {/*
                                      * Two presses, like the account delete: releasing
                                      * a building takes it off somebody who believes it
                                      * is theirs, and there is no undo but asking them
                                      * to claim it again first.
                                      */}
                                    {s.confirming === claim.pand_id ? (
                                        <>
                                            <button
                                                class="danger"
                                                onclick={() => {
                                                    s.confirming = ''
                                                    void releaseClaim(claim)
                                                }}
                                                type="button"
                                            >
                                                Release
                                            </button>
                                            <button
                                                class="ghost"
                                                onclick={() => {
                                                    s.confirming = ''
                                                }}
                                                type="button"
                                            >
                                                Cancel
                                            </button>
                                        </>
                                    ) : (
                                        <button
                                            class="ghost"
                                            onclick={() => {
                                                s.confirming = claim.pand_id
                                            }}
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
}
