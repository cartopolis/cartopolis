/**
 * The simulator's admin API as it fails, not just as it succeeds.
 *
 * Every non-2xx from `http.rs` carries `{"error": "…"}` — a sentence written for
 * a person, because the server is the only place that knows *why* (the last-admin
 * guard, a taken username, an expired session). So this client's job is to get
 * that sentence to the caller intact and to never invent one on top of it.
 */
export class ApiError extends Error {
    status: number

    constructor(status: number, message: string) {
        super(message)
        this.name = 'ApiError'
        this.status = status
    }

    /** A 401 means the token is gone or was never good: sign out, do not retry. */
    get isUnauthorized(): boolean {
        return this.status === 401
    }
}

type Method = 'GET' | 'POST' | 'PATCH' | 'DELETE'

export class Api {
    /**
     * Read at call time rather than captured, so signing in or out takes effect
     * on the next request without rebuilding this object.
     */
    private token: () => string

    /** Called on any 401, so one place decides what an expired session means. */
    private onUnauthorized: () => void

    constructor(token: () => string, onUnauthorized: () => void) {
        this.token = token
        this.onUnauthorized = onUnauthorized
    }

    async get<T>(endpoint: string): Promise<T> {
        return this.request<T>('GET', endpoint)
    }

    async post<T>(endpoint: string, data?: unknown): Promise<T> {
        return this.request<T>('POST', endpoint, data)
    }

    async patch<T>(endpoint: string, data: unknown): Promise<T> {
        return this.request<T>('PATCH', endpoint, data)
    }

    async delete<T>(endpoint: string): Promise<T> {
        return this.request<T>('DELETE', endpoint)
    }

    private async request<T>(method: Method, endpoint: string, data?: unknown): Promise<T> {
        const headers: Record<string, string> = {}
        const token = this.token()
        if (token) {
            headers.Authorization = `Bearer ${token}`
        }
        if (data !== undefined) {
            headers['Content-Type'] = 'application/json'
        }

        let response: Response
        try {
            response = await fetch(endpoint, {
                body: data === undefined ? undefined : JSON.stringify(data),
                // Same-origin by construction: Caddy proxies /api to the
                // simulator, so there is no cross-origin request to make and no
                // CORS to configure.
                credentials: 'same-origin',
                headers,
                method,
            })
        } catch {
            // `fetch` rejects only on a transport failure, which from a browser is
            // indistinguishable between offline, DNS and a dead upstream.
            throw new ApiError(0, 'Could not reach the simulator')
        }

        if (response.status === 401) {
            this.onUnauthorized()
            throw new ApiError(401, 'That session has expired — sign in again')
        }

        // 204, and any empty body: there is nothing to parse and asking `json()`
        // for one throws.
        const body = response.status === 204 ? null : await this.parse(response)

        if (!response.ok) {
            const message = (body as {error?: string} | null)?.error
            throw new ApiError(response.status, message || `The server answered ${response.status}`)
        }
        return body as T
    }

    private async parse(response: Response): Promise<unknown> {
        const text = await response.text()
        if (!text) {
            return null
        }
        try {
            return JSON.parse(text) as unknown
        } catch {
            // An HTML error page from a proxy in front of the simulator: real, and
            // not something to show a user 900 characters of.
            return {error: 'The server sent something this app could not read'}
        }
    }
}
