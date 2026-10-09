/**
 * What crosses between a page and the chat worker, and what the engine reports.
 *
 * `ChatEvent` describes the JSON `crates/web_chat/src/lib.rs` (`event_json`)
 * writes. It is hand-written, like `src/types.ts`; `chat-e2e.ts` reads real
 * events through it against a real simulator, which is what catches a drift.
 */

/** How the worker signs in: the session token the web UI already holds. */
export type Credential = {token: string} | {password: string; username: string}

export type ChatEvent =
    | {address: string; kind: 'signed_in'; server_name: string; session_token: string; username: string}
    | {code: number; kind: 'refused'; message: string; request_id: number}
    | {channel: string; kind: 'opened'; peer: string}
    | {
        body: string
        channel: string
        from: string
        kind: 'line'
        /** An ActivityStreams Note with a Place, when the line is a point. */
        point: string | null
        picture: {hash: string; height: number; key: string; nonce: string; width: number} | null
        sent_at_ms: number
    }
    | {
        accuracy_m: number
        channel: string
        from: string
        kind: 'location'
        latitude: number
        longitude: number
        until_ms: number
    }
    | {channel: string; from: string; kind: 'location_stopped'}
    | {channel: string; kind: 'named'; name: string}
    | {kind: 'notice'; text: string}
    /** Not the engine's: the connection reporting on itself. */
    | {kind: 'disconnected'}
    | {kind: 'error'; message: string}
    /** Another tab or window holds this browser's chat keys. */
    | {kind: 'waiting_for_other_tab'}

export interface ChatSummary {
    channel: string
    group: boolean
    label: string
    people: string[]
}

/** Page → worker. */
export type Command =
    | {credential: Credential; type: 'sign_in'}
    | {type: 'sign_out'}
    | {type: 'start_direct'; username: string}
    | {members: string[]; name: string; type: 'start_group'}
    | {body: string; channel: string; type: 'send_text'}
    | {channel: string; point: string; type: 'send_point'}

/** Worker → page. */
export type Update =
    | {event: ChatEvent; type: 'event'}
    | {chats: ChatSummary[]; type: 'chats'; username: string | null}
