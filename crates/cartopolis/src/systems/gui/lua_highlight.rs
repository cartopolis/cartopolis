//! Syntax colouring for the script editor's Luau.
//!
//! A hand-written lexer rather than `egui_extras::syntax_highlighting`: that
//! module's built-in highlighter knows a handful of languages and Lua is not one
//! of them, and its full one is `syntect` — a regex engine and a grammar bundle
//! in every build, phone included, for one text box. Colouring needs no parse,
//! only the token boundaries, and Lua's are few: comments, three kinds of
//! string, numbers, words.
//!
//! It never fails. Source being typed is source that does not compile most of the
//! time — an unclosed string or long comment simply colours to the end, which is
//! also what tells the author it is unclosed.

use std::ops::Range;

use crate::systems::theme;

/// What a run of source is, for colouring.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Token {
    Plain,
    Keyword,
    /// Numbers and `true` / `false` / `nil`.
    Literal,
    String,
    Comment,
    /// A host function or handler from [`cartopolis_core::SCRIPT_API`] /
    /// `SCRIPT_EVENTS` — the names an author most needs to see spelled right.
    Api,
    /// Any other name immediately followed by `(`.
    Call,
}

const KEYWORDS: &[&str] = &[
    "and", "break", "continue", "do", "else", "elseif", "end", "export", "for", "function", "if",
    "in", "local", "not", "or", "repeat", "return", "then", "type", "until", "while",
];

const LITERALS: &[&str] = &["true", "false", "nil"];

fn is_api(word: &str) -> bool {
    cartopolis_core::SCRIPT_API
        .iter()
        .chain(cartopolis_core::SCRIPT_EVENTS)
        .any(|f| f.name == word)
}

/// The level of a long bracket opening at `i` (`[[` is 0, `[==[` is 2), or
/// `None` if `[` there does not open one.
fn long_bracket(b: &[u8], i: usize) -> Option<usize> {
    if b.get(i) != Some(&b'[') {
        return None;
    }
    let mut j = i + 1;
    while b.get(j) == Some(&b'=') {
        j += 1;
    }
    (b.get(j) == Some(&b'[')).then_some(j - i - 1)
}

/// The end of a long bracket body of `level` starting at `from`: just past its
/// `]==]`, or the end of the source if it is never closed.
fn long_bracket_end(b: &[u8], from: usize, level: usize) -> usize {
    let mut i = from;
    while i < b.len() {
        if b[i] == b']' {
            let mut j = i + 1;
            while b.get(j) == Some(&b'=') {
                j += 1;
            }
            if j - i - 1 == level && b.get(j) == Some(&b']') {
                return j + 1;
            }
        }
        i += 1;
    }
    b.len()
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

    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        let start = i;
        if c == b'-' && b.get(i + 1) == Some(&b'-') {
            i += 2;
            if let Some(level) = long_bracket(b, i) {
                i = long_bracket_end(b, i + level + 2, level);
            } else {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            push(start..i, Token::Comment);
        } else if let Some(level) = long_bracket(b, i) {
            i = long_bracket_end(b, i + level + 2, level);
            push(start..i, Token::String);
        } else if c == b'"' || c == b'\'' || c == b'`' {
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
        } else if c.is_ascii_digit() || (c == b'.' && b.get(i + 1).is_some_and(u8::is_ascii_digit))
        {
            // Loose on purpose: `0x1F`, `1e-3`, `1_000` and `3.14` all end at the
            // first byte that cannot continue a number.
            i += 1;
            while i < b.len() {
                let d = b[i];
                let exponent_sign = (d == b'-' || d == b'+')
                    && matches!(b[i - 1], b'e' | b'E' | b'p' | b'P')
                    && !src[start..i].starts_with("0x")
                    && !src[start..i].starts_with("0X");
                if d.is_ascii_alphanumeric() || d == b'.' || d == b'_' || exponent_sign {
                    i += 1;
                } else {
                    break;
                }
            }
            push(start..i, Token::Literal);
        } else if c.is_ascii_alphabetic() || c == b'_' {
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            let word = &src[start..i];
            // A field (`math.sin`, `s:match`) is never a keyword or a host call.
            let field = start > 0 && matches!(b[start - 1], b'.' | b':');
            let token = if KEYWORDS.contains(&word) {
                Token::Keyword
            } else if LITERALS.contains(&word) {
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
            push(start..i, token);
        } else {
            // Anything else — operators, whitespace, non-ASCII — up to the next
            // byte that could start a token.
            i += 1;
            while i < b.len()
                && !(b[i].is_ascii_alphanumeric()
                    || matches!(b[i], b'_' | b'"' | b'\'' | b'`' | b'[' | b'-' | b'.'))
            {
                i += 1;
            }
            // Never split inside a multi-byte char.
            while !src.is_char_boundary(i) {
                i += 1;
            }
            push(start..i, Token::Plain);
        }
    }
    out
}

fn color(token: Token) -> egui::Color32 {
    match token {
        Token::Plain => theme::TEXT,
        Token::Keyword => theme::CODE_KEYWORD,
        Token::Literal => theme::CODE_LITERAL,
        Token::String => theme::CODE_STRING,
        Token::Comment => theme::CODE_COMMENT,
        Token::Api => theme::CODE_API,
        Token::Call => theme::CODE_CALL,
    }
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
            "local x = 1",
            "s = \"unterminated\nnext line",
            "--[==[ never closed",
            "x = [[ long\nstring ]] .. 'é' -- naïve",
            "a = 0x1F + 1e-3 + .5 + 3.14\n",
            "glyph = { [\"0\"] = \"abc\", [x] = y }",
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
        let src =
            "local n = name():match(\"^seg(%d)$\") -- slot\nif not n then return nil end\nfoo(1)";
        assert_eq!(
            tokens(src),
            vec![
                ("local", Token::Keyword),
                ("name", Token::Call),
                ("match", Token::Call),
                ("\"^seg(%d)$\"", Token::String),
                ("-- slot", Token::Comment),
                ("if", Token::Keyword),
                ("not", Token::Keyword),
                ("then", Token::Keyword),
                ("return", Token::Keyword),
                ("nil", Token::Literal),
                ("end", Token::Keyword),
                ("foo", Token::Call),
                ("1", Token::Literal),
            ]
        );
    }

    #[test]
    fn host_functions_are_named_even_without_a_call() {
        // The editor's own example aliases them: `local set_color = set_color`.
        let name = cartopolis_core::SCRIPT_API[0].name;
        let src = format!("local f = {name}");
        assert!(tokens(&src).contains(&(name, Token::Api)));
        // …but not as a field of something else.
        let src = format!("t.{name}()");
        assert!(!tokens(&src).contains(&(name, Token::Api)));
    }

    #[test]
    fn long_brackets_match_their_level() {
        let src = "--[=[ a ]] b ]=] x";
        assert_eq!(tokens(src)[0], ("--[=[ a ]] b ]=]", Token::Comment));
        let src = "s = [[ \"q\" ]] t";
        assert!(tokens(src).contains(&("[[ \"q\" ]]", Token::String)));
    }

    #[test]
    fn an_escaped_quote_does_not_close_a_string() {
        assert_eq!(tokens(r#""a\"b" c"#)[0], (r#""a\"b""#, Token::String));
    }

    #[test]
    fn numbers_take_exponents_and_hex() {
        let t = tokens("1e-3 0x1F-2 2.5");
        assert!(t.contains(&("1e-3", Token::Literal)));
        // A hex digit `e`/`f` is not an exponent: `0x1F-2` is a subtraction.
        assert!(t.contains(&("0x1F", Token::Literal)));
        assert!(t.contains(&("2", Token::Literal)));
        assert!(t.contains(&("2.5", Token::Literal)));
    }
}
