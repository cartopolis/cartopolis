import m, {MithrilComponent} from '@garage44/mithril'
import {Button, Table} from '@garage44/mithril/components'

import type {OwnedBuilding} from '@/types'

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

        /*
         * Two presses, like the account delete: releasing a building takes it off
         * somebody who believes it is theirs, and there is no undo but asking them
         * to claim it again first.
         */
        const release = (claim: OwnedBuilding) =>
            s.confirming === claim.pand_id ? (
                <span class="claims-actions">
                    <Button
                        onclick={() => {
                            s.confirming = ''
                            void releaseClaim(claim)
                        }}
                        size="sm"
                        variant="danger"
                    >
                        Release
                    </Button>
                    <Button
                        onclick={() => {
                            s.confirming = ''
                        }}
                        size="sm"
                        variant="ghost"
                    >
                        Cancel
                    </Button>
                </span>
            ) : (
                <Button
                    onclick={() => {
                        s.confirming = claim.pand_id
                    }}
                    size="sm"
                    variant="ghost"
                >
                    Release
                </Button>
            )

        return (
            <div class="claims">
                <p class="claims-count">
                    {claims.length} {claims.length === 1 ? 'building' : 'buildings'} claimed
                </p>
                <Table
                    columns={[
                        {key: 'address', label: 'Address'},
                        {key: 'username', label: 'Owner', cell: (c: OwnedBuilding) => c.username || '[deleted]'},
                        {key: 'claimed_at', label: 'Claimed', cell: (c: OwnedBuilding) => formatTimestamp(c.claimed_at)},
                        // The join key to the geometry, and the thing to quote when a
                        // claim has to be traced to a building.
                        {
                            key: 'pand_id',
                            label: 'Building',
                            cell: (c: OwnedBuilding) => <span class="claims-pand">{c.pand_id}</span>,
                        },
                        {key: 'release', label: '', align: 'end', cell: release},
                    ]}
                    empty="Nobody has claimed a building yet."
                    rowKey={(c: OwnedBuilding) => c.pand_id}
                    rows={claims}
                />
            </div>
        )
    }
}
