//! Syntax colouring for the script editor's JavaScript.
//!
//! A hand-written lexer rather than `egui_extras::syntax_highlighting`: its full
//! highlighter is `syntect` — a regex engine and a grammar bundle in every build,
//! phone included, for one text box. Colouring needs no parse, only the token
//! boundaries: comments, three kinds of string (template literals with their
//! `${…}` holes), regular expressions, numbers, words.
//!
//! It never fails. Source being typed is source that does not compile most of the
//! time — an unclosed string or block comment simply colours to the end, which is
//! also what tells the author it is unclosed.

use std::ops::Range;

use crate::systems::theme;

/// What a run of source is, for colouring.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Token {
    Plain,
    Keyword,
    /// Numbers and `true` / `false` / `null` / `undefined`.
    Literal,
    /// Strings, template text and regular expressions.
    String,
    Comment,
    /// A host function or handler from [`cartopolis_core::SCRIPT_API`] /
    /// `SCRIPT_EVENTS` — the names an author most needs to see spelled right.
    Api,
    /// Any other name immediately followed by `(`.
    Call,
}

const KEYWORDS: &[&str] = &[
    "async",
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "default",
    "delete",
    "do",
    "else",
    "export",
    "extends",
    "finally",
    "for",
    "function",
    "if",
    "import",
    "in",
    "instanceof",
    "let",
    "new",
    "of",
    "return",
    "static",
    "super",
    "switch",
    "this",
    "throw",
    "try",
    "typeof",
    "var",
    "void",
    "while",
    "with",
    "yield",
];

const LITERALS: &[&str] = &["true", "false", "null", "undefined", "NaN", "Infinity"];

fn is_api(word: &str) -> bool {
    cartopolis_core::SCRIPT_API
        .iter()
        .chain(cartopolis_core::SCRIPT_EVENTS)
        .any(|f| f.name == word)
}

/// The end of a template literal's text starting at `from` (just past its
/// opening backtick, or past the `}` closing a hole): just past the closing
/// backtick, with `false`; or just past a `${`, with `true` — the text goes on
/// after the hole's matching `}`.
fn template_end(b: &[u8], from: usize) -> (usize, bool) {
    let mut i = from;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            b'`' => return (i + 1, false),
            b'$' if b.get(i + 1) == Some(&b'{') => return (i + 2, true),
            _ => i += 1,
        }
    }
    (b.len(), false)
}

/// The end of a regular expression whose body starts at `from` (just past its
/// opening `/`): past its closing `/` and flags, or the end of the line if it is
/// never closed. A `/` inside a `[…]` class does not close it.
fn regex_end(b: &[u8], from: usize) -> usize {
    let mut i = from;
    let mut class = false;
    while i < b.len() && b[i] != b'\n' {
        match b[i] {
            b'\\' => {
                i += 2;
                continue;
            }
            b'[' => class = true,
            b']' => class = false,
            b'/' if !class => {
                i += 1;
                while i < b.len() && b[i].is_ascii_alphabetic() {
                    i += 1;
                }
                return i;
            }
            _ => {}
        }
        i += 1;
    }
    i.min(b.len())
}

/// Split `src` into coloured runs. The runs are contiguous, in order, and cover
/// every byte; each boundary is a char boundary (every split is at an ASCII byte).
#[must_use]
pub fn spans(src: &str) -> Vec<(Range<usize>, Token)> {
    let b = src.as_bytes();
    let mut out: Vec<(Range<usize>, Token)> = Vec::new();
    let mut push = |range: Range<usize>, token: Token| {
        if range.is_empty() {
            return;
        }
        // Merge neighbours of one kind, so a line of punctuation is one run.
        if let Some((last, t)) = out.last_mut() {
            if *t == token && last.end == range.start {
                last.end = range.end;
                return;
            }
        }
        out.push((range, token));
    };

    // The `${…}` holes open in template literals, innermost last, each with how
    // many `{` deep it is — so the `}` that closes the hole is told from one
    // that closes an object or a block inside it.
    let mut holes: Vec<usize> = Vec::new();
    // Whether a `/` here starts a regular expression rather than dividing:
    // after an operator, an opening bracket or a keyword such as `return`, yes;
    // after a value, no. A guess from the previous token, which is all a lexer
    // without a parser has, and right for the code people write.
    let mut regex_ok = true;

    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        let start = i;
        if c == b'/' && b.get(i + 1) == Some(&b'/') {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            push(start..i, Token::Comment);
        } else if c == b'/' && b.get(i + 1) == Some(&b'*') {
            i = src[i + 2..].find("*/").map_or(b.len(), |at| i + 2 + at + 2);
            push(start..i, Token::Comment);
        } else if c == b'/' && regex_ok {
            i = regex_end(b, i + 1);
            push(start..i, Token::String);
            regex_ok = false;
        } else if c == b'`' || (c == b'}' && holes.last() == Some(&0)) {
            // A template's text, from its opening backtick or from the `}`
            // that closes a hole, up to its end or the next hole.
            if c == b'}' {
                holes.pop();
            }
            let (end, hole) = template_end(b, i + 1);
            i = end;
            if hole {
                holes.push(0);
            }
            regex_ok = hole;
            push(start..i, Token::String);
        } else if c == b'"' || c == b'\'' {
            i += 1;
            while i < b.len() && b[i] != c && b[i] != b'\n' {
                // An escape skips the next byte, so `\"` does not close. Stepping
                // one byte into a multi-byte char is fine: no continuation byte
                // is a quote or a newline.
                i += if b[i] == b'\\' { 2 } else { 1 };
            }
            if b.get(i) == Some(&c) {
                i += 1;
            }
            i = i.min(b.len());
            push(start..i, Token::String);
            regex_ok = false;
        } else if c.is_ascii_digit() || (c == b'.' && b.get(i + 1).is_some_and(u8::is_ascii_digit))
        {
            // Loose on purpose: `0x1F`, `1e-3`, `1_000`, `10n` and `3.14` all end
            // at the first byte that cannot continue a number.
            i += 1;
            while i < b.len() {
                let d = b[i];
                let exponent_sign = (d == b'-' || d == b'+')
                    && matches!(b[i - 1], b'e' | b'E')
                    && !src[start..i].starts_with("0x")
                    && !src[start..i].starts_with("0X");
                if d.is_ascii_alphanumeric() || d == b'.' || d == b'_' || exponent_sign {
                    i += 1;
                } else {
                    break;
                }
            }
            push(start..i, Token::Literal);
            regex_ok = false;
        } else if c.is_ascii_alphabetic() || c == b'_' || c == b'$' {
            while i < b.len() && (b[i].is_ascii_alphanumeric() || matches!(b[i], b'_' | b'$')) {
                i += 1;
            }
            let word = &src[start..i];
            // A property (`Math.sin`, `map.delete`) is never a keyword, a
            // literal or a host call, whatever it is called.
            let field = start > 0 && b[start - 1] == b'.';
            let token = if !field && KEYWORDS.contains(&word) {
                Token::Keyword
            } else if !field && LITERALS.contains(&word) {
                Token::Literal
            } else if !field && is_api(word) {
                Token::Api
            } else if b[i..]
                .iter()
                .find(|x| !matches!(x, b' ' | b'\t'))
                .is_some_and(|&x| x == b'(')
            {
                Token::Call
            } else {
                Token::Plain
            };
            // `return /x/` is a regular expression; `this / 2` divides.
            regex_ok = token == Token::Keyword && !matches!(word, "this" | "super");
            push(start..i, token);
        } else {
            // Anything else — operators, whitespace, non-ASCII — up to the next
            // byte that could start a token. A brace is a run of its own, so a
            // hole's depth is counted at every one.
            match c {
                b'{' => {
                    if let Some(depth) = holes.last_mut() {
                        *depth += 1;
                    }
                }
                b'}' => {
                    if let Some(depth) = holes.last_mut() {
                        *depth -= 1;
                    }
                }
                _ => {}
            }
            i += 1;
            while i < b.len()
                && !matches!(c, b'{' | b'}')
                && !(b[i].is_ascii_alphanumeric()
                    || matches!(
                        b[i],
                        b'_' | b'$' | b'"' | b'\'' | b'`' | b'/' | b'.' | b'{' | b'}'
                    ))
            {
                i += 1;
            }
            // Never split inside a multi-byte char.
            while !src.is_char_boundary(i) {
                i += 1;
            }
            if let Some(&last) = b[start..i].iter().rev().find(|x| !x.is_ascii_whitespace()) {
                regex_ok = !matches!(last, b')' | b']' | b'}');
            }
            push(start..i, Token::Plain);
        }
    }
    out
}

fn color(token: Token) -> egui::Color32 {
    match token {
        Token::Plain => theme::DARK.text,
        Token::Keyword => theme::CODE_KEYWORD,
        Token::Literal => theme::CODE_LITERAL,
        Token::String => theme::CODE_STRING,
        Token::Comment => theme::CODE_COMMENT,
        Token::Api => theme::CODE_API,
        Token::Call => theme::CODE_CALL,
    }
    .into()
}

/// The coloured layout of `src`, for a `TextEdit::layouter`.
#[must_use]
pub fn layout_job(src: &str, font_id: egui::FontId, wrap_width: f32) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = wrap_width;
    for (range, token) in spans(src) {
        job.append(
            &src[range],
            0.0,
            egui::TextFormat {
                font_id: font_id.clone(),
                color: color(token),
                italics: token == Token::Comment,
                ..Default::default()
            },
        );
    }
    job
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The coloured runs of `src`. Plain runs are left out: they merge with the
    /// whitespace around them, so their exact extent says nothing worth pinning.
    fn tokens(src: &str) -> Vec<(&str, Token)> {
        spans(src)
            .into_iter()
            .filter(|(_, t)| *t != Token::Plain)
            .map(|(r, t)| (&src[r], t))
            .collect()
    }

    /// The runs must tile the source exactly, or text disappears from (or is
    /// duplicated in) the editor — which would be a far worse bug than no colour.
    #[test]
    fn spans_cover_every_byte_in_order() {
        for src in [
            "",
            "const x = 1",
            "s = \"unterminated\nnext line",
            "/* never closed",
            "x = `long\n${a + `in ${b}`} text` + 'é' // naïve",
            "`${",
            "}}`",
            "a = 0x1F + 1e-3 + .5 + 3.14\n",
            "glyph = { \"0\": \"abc\", [x]: y }",
            "a / b / /c[/]d/g.test(e)",
            "/\\",
            "\\\"",
            "ünïcödé = \"\\",
        ] {
            let mut at = 0;
            for (r, _) in spans(src) {
                assert_eq!(r.start, at, "gap or overlap in {src:?}");
                assert!(
                    src.is_char_boundary(r.end),
                    "split inside a char in {src:?}"
                );
                at = r.end;
            }
            assert_eq!(at, src.len(), "source not fully covered: {src:?}");
        }
    }

    #[test]
    fn colours_the_shapes_a_script_has() {
        let src = "const m = name().match(/^seg(\\d)$/) // slot\nif (!m) return null\nfoo(1)";
        assert_eq!(
            tokens(src),
            vec![
                ("const", Token::Keyword),
                ("name", Token::Call),
                ("match", Token::Call),
                ("/^seg(\\d)$/", Token::String),
                ("// slot", Token::Comment),
                ("if", Token::Keyword),
                ("return", Token::Keyword),
                ("null", Token::Literal),
                ("foo", Token::Call),
                ("1", Token::Literal),
            ]
        );
    }

    #[test]
    fn host_functions_are_named_even_without_a_call() {
        // A script can alias them: `const paint = set_color`.
        let name = cartopolis_core::SCRIPT_API[0].name;
        let src = format!("const f = {name}");
        assert!(tokens(&src).contains(&(name, Token::Api)));
        // …but not as a property of something else.
        let src = format!("t.{name}()");
        assert!(!tokens(&src).contains(&(name, Token::Api)));
        // Nor is a property named like a keyword a keyword.
        assert_eq!(tokens("seen.delete(id)"), vec![("delete", Token::Call)]);
    }

    #[test]
    fn a_template_is_text_around_its_holes() {
        let src = "`a ${ {k: 1}.k } c` / 2";
        assert_eq!(
            tokens(src),
            vec![
                ("`a ${", Token::String),
                ("1", Token::Literal),
                ("} c`", Token::String),
                ("2", Token::Literal),
            ]
        );
        // A template inside a hole, and a newline in the text.
        let src = "`x ${`in ${n}`}\ny`";
        assert_eq!(
            tokens(src),
            vec![("`x ${`in ${", Token::String), ("}`}\ny`", Token::String)]
        );
    }

    #[test]
    fn a_slash_divides_after_a_value_and_starts_a_regex_after_an_operator() {
        assert!(tokens("a / b / 2").iter().all(|(_, t)| *t != Token::String));
        assert!(tokens("(n) / 2").iter().all(|(_, t)| *t != Token::String));
        assert!(tokens("x = /b\\/[/]/g").contains(&("/b\\/[/]/g", Token::String)));
        assert!(tokens("return /a/").contains(&("/a/", Token::String)));
    }

    #[test]
    fn block_comments_end_at_the_first_close() {
        assert_eq!(
            tokens("/* a */ 1 /* b */"),
            vec![
                ("/* a */", Token::Comment),
                ("1", Token::Literal),
                ("/* b */", Token::Comment),
            ]
        );
    }

    #[test]
    fn an_escaped_quote_does_not_close_a_string() {
        assert_eq!(tokens(r#""a\"b" c"#)[0], (r#""a\"b""#, Token::String));
    }

    #[test]
    fn numbers_take_exponents_and_hex() {
        let t = tokens("1e-3 0x1E-2 2.5");
        assert!(t.contains(&("1e-3", Token::Literal)));
        // A hex digit `e` is not an exponent: `0x1E-2` is a subtraction.
        assert!(t.contains(&("0x1E", Token::Literal)));
        assert!(t.contains(&("2", Token::Literal)));
        assert!(t.contains(&("2.5", Token::Literal)));
    }
}
