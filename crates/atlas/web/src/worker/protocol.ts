/**
 * What crosses between a page and the chat worker, and what the engine reports.
 *
 * `ChatEvent` describes the JSON `crates/web_chat/src/lib.rs` (`event_json`)
 * writes. It is hand-written, like `src/types.ts`; `chat-e2e.ts` reads real
 * events through it against a real simulator, which is what catches a drift.
 */
import type {Message} from './history.ts'

/** How the worker signs in: the session token the web UI already holds. */
export type Credential = {token: string} | {password: string; username: string} | {guest: {code: string; name: string}}

export type Role = 'member' | 'moderator' | 'owner'
export type JoinRule = 'ask' | 'invite' | 'open'

/** A place a community is pinned to, for Near you. */
export interface CommunityPlace {
    latitude: number
    longitude: number
    name: string
}

export interface CommunitySettings {
    about: string
    join: JoinRule
    listed: boolean
    members_start_topics: boolean
    name: string
    /** The hash of its picture in the picture store (a plain JPEG: it is public). */
    picture: string | null
    place: CommunityPlace | null
}

export interface TopicView {
    archived: boolean
    /** The topic's chat, as the same `dm:<hex>` key every chat has. */
    channel: string
    /** Whether this account is in it yet (a member's device adds it). */
    joined: boolean
    name: string
    position: number
}

export interface CommunityReport {
    channel: string
    created_ms: number
    id: number
    line: string
    note: string
    reason: string
    reported: string
    reporter: string
    seq: number
}

export interface CommunityView {
    home: string
    id: string
    members: {address: string; role: Role}[]
    reports: CommunityReport[]
    requests: {address: string; message: string; requested_ms: number}[]
    /** `null` while this account's request to join waits for a moderator. */
    role: Role | null
    settings: CommunitySettings
    topics: TopicView[]
}

export interface CommunityListing {
    about: string
    created_ms: number
    id: string
    join: JoinRule
    members: number
    mine: 'asked' | 'member' | 'outside' | 'refused'
    moderators: {address: string; role: Role}[]
    name: string
    topics: string[]
    picture: string | null
    place: CommunityPlace | null
    /** How far away its place is, for a Near you search. */
    distance_m: number | null
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
    | {communities: CommunityView[]; kind: 'communities'}
    | {id: string; kind: 'community_created'}
    | {kind: 'directory'; listings: CommunityListing[]}
    /** This account's contacts; `online` is null for one who does not show it. */
    | {contacts: {address: string; online: boolean | null}[]; kind: 'contacts'}
    | {code: string; id: string; kind: 'invite'}
    | {by: string; channel: string; kind: 'redacted'; seq: number}
    | {by: string; channel: string; community: string; kind: 'left'}
    | {channel: string; kind: 'history_wanted'}
    | {
          channel: string
          kind: 'call_roster'
          members: {
              device: string
              hand: number | null
              held: boolean
              muted: boolean
              participant: number
              role: 'host' | 'cohost' | 'member'
              sharing: boolean
              username: string
          }[]
          settings: {locked: boolean; may_share: boolean; may_unmute: boolean}
      }
    | {answer: string; channel: string; kind: 'call_joined'; participant: number}
    | {
          channel: string
          kind: 'call_offer'
          sdp: string
          tracks: {kind: 'audio' | 'camera' | 'screen'; mid: string; participant: number; video: boolean}[]
      }
    | {channel: string; kind: 'call_declined'; username: string}
    | {code: number; control: boolean; kind: 'call_refused'; message: string}
    | {by: string; channel: string; kind: 'call_ended'; reason: 'removed' | 'ended'}
    | {channel: string; epoch: string; kind: 'call_reacted'; participant: number; sealed: string}
    | {channel: string; code: string; expires_ms: number; kind: 'call_invited'}
    | {channel: string; kind: 'call_key_dropped'; sealed: string}
    | {
        channel: string
        count: number
        from: string
        kind: 'shared_history'
        picture: {hash: string; height: number; key: string; nonce: string; width: number}
    }
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
    /** A place, or a meet-up's "I'm going", with the words a reader that shows only text sees. */
    | {body: string; channel: string; point: string; type: 'send_point'}
    | {body: string; channel: string; object: string; type: 'send_meeting'}
    | {channel: string; emoji: string; remove: boolean; target: number; type: 'react'}
    | {body: string; channel: string; target: number; type: 'edit'}
    | {channel: string; target: number; type: 'delete'}
    | {people: string[]; settings: CommunitySettings; topics: string[]; type: 'create_community'}
    | {id: string; settings: CommunitySettings; type: 'edit_community'}
    | {
        latitude: number | null
        longitude: number | null
        order: 'near' | 'new' | 'popular'
        query: string
        type: 'find_communities'
    }
    | {id: string; type: 'create_invite'}
    | {code: string; type: 'lookup_invite'}
    | {code: string; type: 'join_invite'}
    | {id: string; message: string; type: 'join_community'}
    | {id: string; type: 'leave_community'}
    | {accept: boolean; id: string; type: 'decide_request'; who: string}
    | {id: string; type: 'add_to_community'; who: string}
    | {id: string; role: Role; type: 'set_community_role'; who: string}
    | {block: boolean; channel: string; id: string; type: 'remove_from_community'; who: string}
    | {id: string; type: 'delete_community'}
    | {id: string; name: string; type: 'start_topic'}
    | {archived: boolean; channel: string; id: string; name: string; position: number; type: 'edit_topic'}
    | {channel: string; target: number; type: 'moderate_delete'}
    | {channel: string; line: string; note: string; reason: string; target: number; type: 'report_to_moderators'; who: string}
    | {id: string; report: number; type: 'resolve_report'}
    | {channel: string; type: 'add_to_chat'; who: string}
    | {channel: string; offer: string; screen: string; type: 'call_join'}
    /** A hand, a shared screen or a host's action, as `CallAction`'s JSON. */
    | {action: string; channel: string; type: 'call_control'}
    /** A reaction sealed by `call-events.js`, hex, for `epoch` (decimal). */
    | {channel: string; epoch: string; sealed: string; type: 'call_react'}
    /** Whose cameras this page draws: a JSON list of participants, or `null`. */
    | {cameras: string; channel: string; type: 'call_watch'}
    | {channel: string; type: 'call_invite'}
    | {channel: string; participant: number; sealed: string; type: 'call_key_drop'}
    | {channel: string; sdp: string; type: 'call_answer'}
    | {channel: string; type: 'call_leave'}
    | {channel: string; type: 'call_decline'}
    /** This browser's push subscription (`webpush:` + its URL), or '' to take it back. */
    | {endpoint: string; type: 'push_endpoint'}
    /** The chat's call key for its current epoch, answered as `call_keys`. */
    | {channel: string; id: number; type: 'call_keys'}
    /** The page is showing this chat: send its history, and count it read. */
    | {channel: string; type: 'open'}
    /** Send every chat's history, read or not (the calendar gathers plans from all of them). */
    | {type: 'histories'}

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
        communities: CommunityView[]
        directory: CommunityListing[]
        /** Which contacts are signed in, by full address; one who hides it is absent. */
        online: Record<string, boolean>
    }
    | {channel: string; messages: Message[]; type: 'history'}
    | {id: number; keys: {epoch: number; index: number; key: string} | null; type: 'call_keys'}
