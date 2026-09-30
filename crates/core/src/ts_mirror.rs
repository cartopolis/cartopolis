//! Just enough TypeScript reading to pin a hand-written copy of a Rust type.
//!
//! # Why this exists
//!
//! The atlas web UI (`crates/atlas/web`) and the autopilot (`tools/autopilot.ts`)
//! describe several Rust types by hand — `auth::AccountRow`, the placeholder
//! table, the placeholder vocabulary, the upload limits, the autopilot's config.
//! Being in one repository does not make those copies checked: a renamed field
//! compiles on both sides and arrives as a column that reads `undefined`. Tests
//! in the crates that own each type `include_str!` the TypeScript and compare it
//! against the Rust through these functions.
//!
//! It is *test support*, and deliberately not a parser. It finds one declaration
//! by name, cuts out its braces or brackets (skipping comments and string
//! literals so neither can unbalance them) and reads the top-level member names.
//! That is all a drift test needs, and anything cleverer would be a second
//! TypeScript implementation to keep correct. One known limit: the declaration
//! is found by text, so a *comment* reading `interface Name` ahead of the real
//! one would be taken for it — name the type in prose, not in declaration form.
//!
//! It lives in `cartopolis_core`
//! because that is the one crate the simulator and atlas both depend on, so the
//! tests share one reader instead of growing one each.

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// The byte offset just past `<keyword> <name>`, where `name` is a whole word.
fn find_decl(src: &str, keyword: &str, name: &str) -> Option<usize> {
    let needle = format!("{keyword} {name}");
    let mut from = 0;
    while let Some(off) = src[from..].find(&needle) {
        let at = from + off;
        let end = at + needle.len();
        let before_ok = src[..at].chars().next_back().is_none_or(|c| !is_ident(c));
        let after_ok = src[end..].chars().next().is_none_or(|c| !is_ident(c));
        if before_ok && after_ok {
            return Some(end);
        }
        from = end;
    }
    None
}

/// The index just past the string literal that opens at `chars[i]`.
fn skip_string(chars: &[char], i: usize) -> usize {
    let quote = chars[i];
    let mut j = i + 1;
    while j < chars.len() {
        if chars[j] == '\\' {
            j += 2;
            continue;
        }
        if chars[j] == quote {
            return j + 1;
        }
        j += 1;
    }
    chars.len()
}

/// What lies between the first `open` at or after `from` and its matching
/// `close`, with comments removed and string literals kept verbatim.
fn block(src: &str, from: usize, open: char, close: char) -> Option<String> {
    let rest = &src[from..];
    let start = rest.find(open)? + open.len_utf8();
    let chars: Vec<char> = rest[start..].chars().collect();
    let mut out = String::new();
    let mut depth = 1;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '/' && chars.get(i + 1) == Some(&'/') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '/' && chars.get(i + 1) == Some(&'*') {
            i += 2;
            while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                i += 1;
            }
            i += 2;
            out.push(' ');
            continue;
        }
        if matches!(c, '\'' | '"' | '`') {
            let end = skip_string(&chars, i);
            out.extend(&chars[i..end]);
            i = end;
            continue;
        }
        if c == open {
            depth += 1;
        } else if c == close {
            depth -= 1;
            if depth == 0 {
                return Some(out);
            }
        }
        out.push(c);
        i += 1;
    }
    None
}

/// The top-level `name: value` / `name?: value` members of a block body, in
/// order, with each value's text trimmed. Nested braces, brackets and
/// parentheses are skipped over, so `progress: {done?: number}` is one member.
fn members(body: &str) -> Vec<(String, String)> {
    let chars: Vec<char> = body.chars().collect();
    let mut out: Vec<(String, String)> = Vec::new();
    let mut current: Option<usize> = None;
    let mut depth = 0i32;
    let mut at_start = true;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if depth == 0 && at_start {
            if c.is_whitespace() {
                i += 1;
                continue;
            }
            at_start = false;
            let s = i;
            while i < chars.len() && is_ident(chars[i]) {
                i += 1;
            }
            if i > s {
                let mut j = i;
                if chars.get(j) == Some(&'?') {
                    j += 1;
                }
                if chars.get(j) == Some(&':') {
                    out.push((chars[s..i].iter().collect(), String::new()));
                    current = Some(out.len() - 1);
                    i = j + 1;
                }
                continue;
            }
        }
        if matches!(c, '\'' | '"' | '`') {
            let end = skip_string(&chars, i);
            if let Some(k) = current {
                out[k].1.extend(&chars[i..end]);
            }
            i = end;
            continue;
        }
        match c {
            '{' | '[' | '(' => depth += 1,
            '}' | ']' | ')' => depth -= 1,
            '\n' | ';' | ',' if depth == 0 => {
                at_start = true;
                current = None;
                i += 1;
                continue;
            }
            _ => {}
        }
        if let Some(k) = current {
            out[k].1.push(c);
        }
        i += 1;
    }
    for member in &mut out {
        member.1 = member.1.trim().to_string();
    }
    out
}

/// The member names of `interface <name> { … }` or `type <name> = { … }`, in
/// declaration order, or `None` when the source declares no such type.
#[must_use]
pub fn type_fields(src: &str, name: &str) -> Option<Vec<String>> {
    let at = find_decl(src, "interface", name).or_else(|| find_decl(src, "type", name))?;
    let body = block(src, at, '{', '}')?;
    Some(members(&body).into_iter().map(|(n, _)| n).collect())
}

/// The string literals of `const <name> = [ … ]`, in order, or `None` when the
/// source declares no such constant.
#[must_use]
pub fn string_array(src: &str, name: &str) -> Option<Vec<String>> {
    let at = find_decl(src, "const", name)?;
    let body = block(src, at, '[', ']')?;
    let chars: Vec<char> = body.chars().collect();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if matches!(chars[i], '\'' | '"' | '`') {
            let end = skip_string(&chars, i);
            out.push(
                chars[i + 1..end.saturating_sub(1).max(i + 1)]
                    .iter()
                    .collect(),
            );
            i = end;
        } else {
            i += 1;
        }
    }
    Some(out)
}

/// The members of `const <name> = { … }` whose values are whole numbers — a
/// literal with optional `_` separators, or a product of them (`16 * 1024`).
///
/// `None` when the constant is missing or any member's value is something else,
/// because a drift test that silently skipped a value it could not read would
/// pass while checking nothing.
#[must_use]
pub fn object_numbers(src: &str, name: &str) -> Option<Vec<(String, u64)>> {
    let at = find_decl(src, "const", name)?;
    let body = block(src, at, '{', '}')?;
    members(&body)
        .into_iter()
        .map(|(key, value)| {
            let mut product: u64 = 1;
            for factor in value.split('*') {
                let n: u64 = factor.trim().replace('_', "").parse().ok()?;
                product = product.checked_mul(n)?;
            }
            Some((key, product))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
/** A comment mentioning AccountRow, which is not a declaration of it. */
export interface AccountRowX { nope: string }
export interface AccountRow extends Record<string, unknown> {
    /** Braces in a comment: { } and an apostrophe's here. */
    id: string
    email?: null | string
    progress: {done?: number; phase?: string}
    level: 'info' | '}' | 'error'
    tags: string[]; count: number
    handler: (a: number, b: string) => void
}

type Config = {
  enabled: boolean;
  // focus: string;
  focusPlan: PlanItem[];
};

export const KINDS = [
    'lamp', // the obvious one
    "picnic-table",
] as const

export const LIMITS = {
    maxBytes: 16 * 1024 * 1024,
    maxTriangles: 250_000,
} as const

export const BROKEN = {
    maxBytes: SOMETHING,
}
"#;

    #[test]
    fn reads_the_top_level_fields_of_an_interface() {
        assert_eq!(
            type_fields(SAMPLE, "AccountRow").unwrap(),
            ["id", "email", "progress", "level", "tags", "count", "handler"]
        );
        assert_eq!(type_fields(SAMPLE, "AccountRowX").unwrap(), ["nope"]);
        assert_eq!(type_fields(SAMPLE, "Other"), None);
    }

    #[test]
    fn reads_a_type_alias_and_ignores_commented_out_fields() {
        assert_eq!(
            type_fields(SAMPLE, "Config").unwrap(),
            ["enabled", "focusPlan"]
        );
    }

    #[test]
    fn reads_a_string_array() {
        assert_eq!(
            string_array(SAMPLE, "KINDS").unwrap(),
            ["lamp", "picnic-table"]
        );
    }

    #[test]
    fn reads_numbers_and_refuses_what_is_not_one() {
        assert_eq!(
            object_numbers(SAMPLE, "LIMITS").unwrap(),
            [
                ("maxBytes".to_string(), 16 * 1024 * 1024),
                ("maxTriangles".to_string(), 250_000)
            ]
        );
        assert_eq!(object_numbers(SAMPLE, "BROKEN"), None);
    }
}
