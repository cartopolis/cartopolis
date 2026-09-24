import {describe, expect, test} from 'bun:test'

import {tokenFromLocation, whatIsMissing} from '@/lib/signup'

describe('the sign-up form', () => {
    const missing = (username: string, email: string, password: string) =>
        whatIsMissing(username, email, password, 8, 64)

    test('accepts what the server accepts', () => {
        expect(missing('ada', 'ada@example.com', 'correcthorse')).toBeNull()
        // Plus tags and long TLDs are real addresses. A stricter check here can
        // only reject ones that would have worked — the address is about to be
        // tested by sending mail to it, which is a better check than any regex.
        expect(missing('ada', 'ada+carto@example.museum', 'correcthorse')).toBeNull()
        expect(missing('  ada  ', 'ada@example.com', 'correcthorse')).toBeNull()
    })

    test('says which field is wrong, one at a time', () => {
        expect(missing('', 'ada@example.com', 'correcthorse')).toContain('username')
        expect(missing('a'.repeat(65), 'ada@example.com', 'correcthorse')).toContain('64')
        expect(missing('ada', 'ada', 'correcthorse')).toContain('email')
        expect(missing('ada', 'ada@localhost', 'correcthorse')).toContain('email')
        expect(missing('ada', '@example.com', 'correcthorse')).toContain('email')
        expect(missing('ada', 'ada@example.com', 'short')).toContain('8')
    })
})

describe('the verification link', () => {
    test('carries the token in the query, not the path', () => {
        expect(tokenFromLocation('?token=abc123')).toBe('abc123')
        expect(tokenFromLocation('?resend=ada%40example.com&token=abc123')).toBe('abc123')
        // The other way into /verify: the "send another" link, which has an
        // address and no token. The page must read that as "no token" and offer
        // the re-send form rather than confirming something.
        expect(tokenFromLocation('?resend=ada%40example.com')).toBe('')
        expect(tokenFromLocation('')).toBe('')
    })
})
