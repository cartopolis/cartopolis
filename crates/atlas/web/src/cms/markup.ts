/**
 * The little markup a page's text may carry: `**bold**`, `*italic*`,
 * `[words](/path)`, and a blank line between paragraphs. So a translator can
 * put the emphasis and the link where their language puts them, instead of the
 * page splitting one sentence over three keys. Parsed into a tree and drawn as
 * elements, never as HTML, so a text cannot add markup of its own.
 *
 * A marker with no partner is kept as the character it is; `\*`, `\[` and `\\`
 * write the character itself.
 */
export type Inline = string | {b: Inline[]} | {i: Inline[]} | {a: string; ch: Inline[]}

export function paragraphs(text: string): string[] {
    return text
        .split(/\n[ \t]*\n/)
        .map((p) => p.trim())
        .filter(Boolean)
}

export function parseInline(text: string): Inline[] {
    return parseRun(text, 0, null).nodes
}

/** Parse from `start` until `close` (or the end), returning the nodes and where it stopped. */
function parseRun(s: string, start: number, close: string | null): {nodes: Inline[]; end: number; closed: boolean} {
    const nodes: Inline[] = []
    let text = ''
    const flush = () => {
        if (text) nodes.push(text)
        text = ''
    }
    let i = start
    while (i < s.length) {
        if (close && s.startsWith(close, i) && !(close === '*' && s.startsWith('**', i) && !s.startsWith('***', i))) {
            flush()
            return {closed: true, end: i + close.length, nodes}
        }
        const ch = s[i]
        if (ch === '\\' && i + 1 < s.length && '*[]()\\'.includes(s[i + 1])) {
            text += s[i + 1]
            i += 2
            continue
        }
        if (s.startsWith('**', i)) {
            const inner = parseRun(s, i + 2, '**')
            if (inner.closed && inner.nodes.length) {
                flush()
                nodes.push({b: inner.nodes})
                i = inner.end
                continue
            }
        } else if (ch === '*') {
            const inner = parseRun(s, i + 1, '*')
            if (inner.closed && inner.nodes.length) {
                flush()
                nodes.push({i: inner.nodes})
                i = inner.end
                continue
            }
        } else if (ch === '[') {
            const label = parseRun(s, i + 1, ']')
            if (label.closed && s[label.end] === '(') {
                const shut = s.indexOf(')', label.end + 1)
                const href = shut < 0 ? '' : s.slice(label.end + 1, shut).trim()
                if (href && !/\s/.test(href)) {
                    flush()
                    nodes.push({a: href, ch: label.nodes})
                    i = shut + 1
                    continue
                }
            }
        }
        text += ch
        i += 1
    }
    flush()
    return {closed: false, end: i, nodes}
}
