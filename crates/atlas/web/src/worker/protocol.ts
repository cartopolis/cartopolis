/**
 * What crosses between a page and the chat worker, and what the engine reports.
 *
 * `ChatEvent` describes the JSON `crates/web_chat/src/lib.rs` (`event_json`)
 * writes. It is hand-written, like `src/types.ts`; `chat-e2e.ts` reads real
 * events through it against a real simulator, which is what catches a drift.
 */
import type {Message} from './history.ts'

/** How the worker signs in: the session token the web UI already holds. */
export type Credential = {token: string} | {password: string; username: string}

export interface ChannelView {
    /** The channel's chat, as the same `dm:<hex>` key every chat has. */
    channel: string
    joined: boolean
    members: number
    name: string
    pending: boolean
    private: boolean
    topic: string
}

export type Role = 'admin' | 'member' | 'owner'

export interface WorkspaceView {
    channels: ChannelView[]
    home: string
    id: string
    members: {address: string; role: Role}[]
    name: string
    role: Role
}

export type ChatEvent =
    | {address: string; kind: 'signed_in'; server_name: string; session_token: string; username: string}
    | {code: number; kind: 'refused'; message: string; request_id: number}
    | {channel: string; kind: 'opened'; peer: string}
    | {
        body: string
        channel: string
        from: string
        kind: 'line'
        /** The thread this line is in: its parent's `seq`. */
        parent: number | null
        /** An ActivityStreams Note with a Place, when the line is a point. */
        point: string | null
        picture: {hash: string; height: number; key: string; nonce: string; width: number} | null
        sent_at_ms: number
        /** Its place in the chat's log: the message's id. */
        seq: number
    }
    | {channel: string; emoji: string; from: string; kind: 'reaction'; remove: boolean; target: number}
    | {body: string; channel: string; from: string; kind: 'edited'; target: number}
    | {channel: string; from: string; kind: 'deleted'; target: number}
    | {channel: string; kind: 'sent'; request: number; seq: number}
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
    | {kind: 'workspaces'; workspaces: WorkspaceView[]}
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
    /** Messages from other people not yet seen. */
    unread: number
    /** Whether one of those names this person. */
    mentioned: boolean
    /** When the last message was said, for ordering the list. */
    lastAtMs: number
    /** The last message, for the list's preview line. */
    preview: string
}

/** Page → worker. */
export type Command =
    | {credential: Credential; type: 'sign_in'}
    | {type: 'sign_out'}
    | {type: 'start_direct'; username: string}
    | {members: string[]; name: string; type: 'start_group'}
    | {body: string; channel: string; parent: number | null; type: 'send_text'}
    | {channel: string; point: string; type: 'send_point'}
    | {channel: string; emoji: string; remove: boolean; target: number; type: 'react'}
    | {body: string; channel: string; target: number; type: 'edit'}
    | {channel: string; target: number; type: 'delete'}
    | {name: string; type: 'create_workspace'}
    | {type: 'add_workspace_member'; who: string; workspace: string}
    | {name: string; private: boolean; topic: string; type: 'start_channel'; workspace: string}
    | {channel: string; type: 'join_channel'; workspace: string}
    | {channel: string; type: 'add_to_chat'; who: string}
    /** The page is showing this chat: send its history, and count it read. */
    | {channel: string; type: 'open'}

/** Worker → page. */
export type Update =
    | {event: ChatEvent; type: 'event'}
    | {
        address: string | null
        chats: ChatSummary[]
        /** Connected and signed in, signing in, or not. */
        status: 'connected' | 'connecting' | 'signed_out' | 'waiting'
        type: 'state'
        username: string | null
        workspaces: WorkspaceView[]
    }
    | {channel: string; messages: Message[]; type: 'history'}
