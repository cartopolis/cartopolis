/** Types for the shared call module the viewer and the chat page both use. */
declare module '*/cartopolis/web/webcall.js' {
    export function webCallSupported(): boolean
    export class WebCall {
        constructor(
            onEvent: (kind: string, value: string) => void,
            options?: {
                onStream?: (participant: number, kind: 'audio' | 'video', stream: MediaStream | null) => void
                workerUrl?: string
            },
        )
        start(): Promise<void>
        joined(answer: string): Promise<void>
        offered(sdp: string, tracks: {mid: string; participant: number; video: boolean}[]): Promise<void>
        keys(epoch: number, key: Uint8Array, index: number): void
        setMuted(muted: boolean): void
        setCamera(on: boolean): Promise<void>
        selfView(): MediaStream | null
        forgetGone(): void
        close(): void
    }
}
