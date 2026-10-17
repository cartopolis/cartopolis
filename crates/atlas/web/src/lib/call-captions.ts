/**
 * Captions of this device's own voice, made on this device (`docs/notes/calls.md`).
 *
 * Each speaker's device writes their captions, from the clearest copy of the
 * voice there is, and sends the text sealed with the call's key; nobody's
 * voice goes to a speech service. In a browser that means the Web Speech API
 * with `processLocally`, which Chrome serves from a language pack on the
 * device: without it (a browser that would send the audio to its maker's
 * servers) captions are *not offered* — the alternative would quietly undo
 * the call's encryption for the one thing that matters most in it.
 */

/** A line of captions as it goes over the call: who is said by the server. */
export interface CaptionLine {
    /** What was said so far. */
    text: string
    /** Final: this line is done, and belongs in the transcript. */
    final: boolean
}

type Availability = 'available' | 'downloadable' | 'downloading' | 'unavailable'

interface LocalRecognition extends EventTarget {
    continuous: boolean
    interimResults: boolean
    lang: string
    processLocally: boolean
    start(): void
    stop(): void
    onresult: ((e: {resultIndex: number; results: ArrayLike<ArrayLike<{transcript: string}> & {isFinal: boolean}>}) => void) | null
    onend: (() => void) | null
    onerror: ((e: {error: string}) => void) | null
}

interface RecognitionClass {
    new (): LocalRecognition
    available?(o: {langs: string[]; processLocally: boolean}): Promise<Availability>
    install?(o: {langs: string[]; processLocally: boolean}): Promise<boolean>
}

function recognitionClass(): RecognitionClass | null {
    const w = globalThis as unknown as {SpeechRecognition?: RecognitionClass; webkitSpeechRecognition?: RecognitionClass}
    return w.SpeechRecognition ?? null
}

/** Whether this browser can caption on the device, for `lang`; and whether it
 *  first has to fetch the language pack. */
export async function captionsAvailable(lang: string): Promise<Availability> {
    const R = recognitionClass()
    if (!R?.available) return 'unavailable'
    try {
        return await R.available({langs: [lang], processLocally: true})
    } catch {
        return 'unavailable'
    }
}

/** The languages offered, in the order the picker shows them. */
export const CAPTION_LANGUAGES = [
    {code: 'nl-NL', label: 'Nederlands'},
    {code: 'en-US', label: 'English'},
    {code: 'de-DE', label: 'Deutsch'},
    {code: 'fr-FR', label: 'Français'},
]

/** Captions this device's voice while it runs; `onLine` gets each step. */
export class Captioner {
    private r: LocalRecognition
    private stopped = false
    private lastSent = 0

    private constructor(r: LocalRecognition, private onLine: (l: CaptionLine) => void) {
        this.r = r
    }

    /** Start, fetching the language pack first if needed. `null` and a reason
     *  when this browser cannot caption on the device. */
    static async start(lang: string, onLine: (l: CaptionLine) => void): Promise<{captioner: Captioner} | {why: string}> {
        const R = recognitionClass()
        if (!R?.available) return {why: 'This browser cannot make captions on the device, so captions are off.'}
        let state = await captionsAvailable(lang)
        if (state === 'downloadable' && R.install) {
            const ok = await R.install({langs: [lang], processLocally: true}).catch(() => false)
            state = ok ? 'available' : 'unavailable'
        }
        if (state !== 'available') {
            return {why: state === 'downloading' ? 'The language for captions is still downloading.' : 'Captions are not available in this language here.'}
        }
        const r = new R()
        r.lang = lang
        r.continuous = true
        r.interimResults = true
        r.processLocally = true
        const c = new Captioner(r, onLine)
        r.onresult = (e) => c.result(e)
        // Recognition stops on silence; captions run until turned off.
        r.onend = () => {
            if (!c.stopped) r.start()
        }
        r.onerror = (e) => {
            if (e.error === 'not-allowed' || e.error === 'language-not-supported') c.stop()
        }
        r.start()
        return {captioner: c}
    }

    private result(e: {resultIndex: number; results: ArrayLike<ArrayLike<{transcript: string}> & {isFinal: boolean}>}): void {
        const r = e.results[e.results.length - 1]
        if (!r) return
        const text = r[0]?.transcript.trim() ?? ''
        if (!text) return
        // Interim lines at most four a second (each is a fan-out to the call);
        // a final line always.
        const now = Date.now()
        if (!r.isFinal && now - this.lastSent < 250) return
        this.lastSent = now
        this.onLine({final: r.isFinal, text: text.slice(-300)})
    }

    stop(): void {
        this.stopped = true
        this.r.stop()
    }
}
