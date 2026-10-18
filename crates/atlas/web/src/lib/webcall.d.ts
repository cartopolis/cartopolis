/** Types for the shared call module the viewer and the chat page both use. */
declare module '*/cartopolis/web/webcall.js' {
    export function webCallSupported(): boolean
    export class WebCall {
        constructor(
            onEvent: (kind: string, value: string) => void,
            options?: {
                onStream?: (participant: number, kind: 'audio' | 'video' | 'screen', stream: MediaStream | null) => void
                screen?: boolean
                audioDevice?: string
                videoDevice?: string
                workerUrl?: string
            },
        )
        /** The up connection's media id for a shared screen, once offered. */
        screenMid: string | null
        start(): Promise<void>
        joined(answer: string): Promise<void>
        offered(
            sdp: string,
            tracks: {kind?: 'audio' | 'camera' | 'screen'; mid: string; participant: number; video: boolean}[],
        ): Promise<void>
        keys(epoch: number, key: Uint8Array, index: number): void
        setMuted(muted: boolean): void
        setCamera(on: boolean): Promise<void>
        setScreen(on: boolean, sharp?: boolean): Promise<boolean>
        setAudioDevice(deviceId: string | null): Promise<void>
        setVideoDevice(deviceId: string | null): Promise<void>
        blurSupported(): boolean
        setBlur(on: boolean): Promise<boolean>
        selfView(): MediaStream | null
        screenView(): MediaStream | null
        levelTrack(): MediaStreamTrack | null
        forgetGone(): void
        close(): void
    }
}

declare module '*/cartopolis/web/call-events.js' {
    export function sealEvent(baseKey: Uint8Array, epoch: bigint, plain: Uint8Array, nonce?: Uint8Array): Promise<Uint8Array>
    export function openEvent(baseKey: Uint8Array, epoch: bigint, sealed: Uint8Array): Promise<Uint8Array | null>
    export const GUEST_INDEX_BASE: number
    export function guestIndex(participant: number): number
    export interface GuestKey {
        baseKey: Uint8Array
        epoch: bigint
        members: number[]
        ownIndex: number
    }
    export function sealGuestKey(secret: Uint8Array, group: Uint8Array, key: GuestKey, nonce?: Uint8Array): Promise<Uint8Array>
    export function openGuestKey(secret: Uint8Array, group: Uint8Array, sealed: Uint8Array): Promise<GuestKey | null>
}
