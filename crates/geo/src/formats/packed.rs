//! **Packed JSON**: one of these cell documents with its coordinate pairs taken
//! out of the text and written as integer deltas.
//!
//! The per-cell JSON files are mostly `[lat, lng]` pairs at seven decimals, and
//! Brotli over that decimal text leaves about a third of their bytes on the
//! table: on the Grote Markt cell (14/8490/5319, 2026-11-21) the BAG facts went
//! out at 316 KB where the same pairs as deltas and the rest as text came to
//! 203 KB, the BGT paving 444 → 300, the roofs 375 → 296, the registers
//! 197 → 139, the places 148 → 126. That saving is in the *numbers*; the
//! documents' shape, keys and strings are not worth a format of their own, so
//! this does not give each layer a codec the way `lod22_cell` and
//! `osm_buildings` have one. It is **one transform for every JSON cell file**:
//!
//! 1. the document's text, with every two-element array of decimal numbers that
//!    has at most seven decimals replaced by the token `-0.0` — a number no
//!    writer here ever emits (`serde_json` prints an integer zero as `0` and
//!    never a negative zero), and which [`unpack`] finds again by scanning the
//!    text outside its string literals;
//! 2. the pairs, in document order, as zigzag varints of the difference from the
//!    previous pair in units of 10⁻⁷ degrees.
//!
//! [`unpack`] gives back JSON **text** that parses to the same values, not the
//! same bytes: a number comes back in its shortest decimal form, which is what
//! atlas wrote in the first place. The client stores that text in its cache
//! under the layer's ordinary key and parses it with the layer's ordinary
//! parser, so nothing downstream knows the file travelled packed.
//!
//! atlas writes `x.pj` beside every `x.json` cell it writes (`Ctx::write_json`,
//! `write_doc`) when the packed form is smaller, which it is for anything with
//! coordinates in it; the client asks for the `.pj` first and keeps asking the
//! JSON on a host that has none ([`PACKED_SUFFIX`]). A document [`pack`]
//! declines — one that already contains the token, or whose numbers do not come
//! back exactly — is simply served as JSON only.

use serde_json::Value;

const MAGIC: &[u8; 4] = b"CPJ1";

/// What a cell file's `.json` becomes: `bag/14/x/y.json` → `bag/14/x/y.pj`,
/// `x.places.json` → `x.places.pj`.
pub const PACKED_SUFFIX: &str = ".pj";

/// The token a pair leaves in the text. Never written by any JSON writer in
/// this workspace: `serde_json` prints `0`, `0.0` or `-0.0` only for a literal
/// negative zero, which no pipeline produces.
const TOKEN: &[u8] = b"-0.0";

/// Degrees to units: seven decimals, the precision every extractor writes.
const UNIT: f64 = 1e7;

/// The packed address of a JSON cell file, or `None` for a path that is not one.
#[must_use]
pub fn packed_path(json_path: &str) -> Option<String> {
    json_path
        .strip_suffix(".json")
        .map(|stem| format!("{stem}{PACKED_SUFFIX}"))
}

/// Whether `bytes` begin like a packed document — the check before anything is
/// stored or decoded, since a proxy's HTML error page is also a 200.
#[must_use]
pub fn looks_packed(bytes: &[u8]) -> bool {
    bytes.len() > MAGIC.len() && &bytes[..MAGIC.len()] == MAGIC
}

/// Pack a JSON document's text. `None` when the text is not JSON, holds the
/// token already, has no pairs worth taking out, or would not come back as the
/// same values — each of which means "serve it as JSON only".
#[must_use]
pub fn pack(text: &str) -> Option<Vec<u8>> {
    pack_or_why(text).ok()
}

/// [`pack`], with the reason it declined — for a pipeline log or a test.
///
/// # Errors
///
/// The reason, as text.
pub fn pack_or_why(text: &str) -> Result<Vec<u8>, String> {
    let value: Value = serde_json::from_str(text).map_err(|e| format!("not JSON: {e}"))?;
    let mut pairs: Vec<(i64, i64)> = Vec::new();
    let skeleton = take_pairs(value, &mut pairs);
    if pairs.is_empty() {
        return Err("no decimal pairs".into());
    }
    let skeleton = serde_json::to_vec(&skeleton).map_err(|e| e.to_string())?;
    // The token must occur exactly once per pair, or unpack would put a pair
    // where the document had a real number.
    let tokens = count_tokens(&skeleton);
    if tokens != pairs.len() {
        return Err(format!(
            "{tokens} tokens in the text for {} pairs",
            pairs.len()
        ));
    }
    let mut out = Vec::with_capacity(skeleton.len() + pairs.len() * 4 + 16);
    out.extend_from_slice(MAGIC);
    varint(&mut out, skeleton.len() as u64);
    out.extend_from_slice(&skeleton);
    varint(&mut out, pairs.len() as u64);
    let (mut lat, mut lng) = (0i64, 0i64);
    for (a, b) in pairs {
        zigzag(&mut out, a - lat);
        zigzag(&mut out, b - lng);
        (lat, lng) = (a, b);
    }
    // What comes back must parse to what went in.
    let back = unpack(&out).ok_or("does not unpack")?;
    let original: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let returned: Value =
        serde_json::from_str(&back).map_err(|e| format!("unpacked text is not JSON: {e}"))?;
    match first_difference(&original, &returned, String::new()) {
        None => Ok(out),
        Some(path) => Err(format!("comes back different at {path}")),
    }
}

/// The path of the first value that differs between two documents.
fn first_difference(a: &Value, b: &Value, path: String) -> Option<String> {
    match (a, b) {
        (Value::Array(x), Value::Array(y)) => {
            if x.len() != y.len() {
                return Some(format!("{path}: {} against {} items", x.len(), y.len()));
            }
            x.iter()
                .zip(y)
                .enumerate()
                .find_map(|(i, (p, q))| first_difference(p, q, format!("{path}[{i}]")))
        }
        (Value::Object(x), Value::Object(y)) => {
            if x.len() != y.len() {
                return Some(format!("{path}: {} against {} keys", x.len(), y.len()));
            }
            x.iter().find_map(|(k, p)| match y.get(k) {
                Some(q) => first_difference(p, q, format!("{path}.{k}")),
                None => Some(format!("{path}.{k}: missing")),
            })
        }
        _ => (a != b).then(|| format!("{path}: {a} against {b}")),
    }
}

/// The JSON text a packed document stands for, or `None` for bytes that are
/// not one.
#[must_use]
pub fn unpack(bytes: &[u8]) -> Option<String> {
    if !looks_packed(bytes) {
        return None;
    }
    let mut r = Reader {
        bytes,
        at: MAGIC.len(),
    };
    let len = usize::try_from(r.varint()?).ok()?;
    let skeleton = bytes.get(r.at..r.at.checked_add(len)?)?;
    r.at += len;
    let pairs = usize::try_from(r.varint()?).ok()?;
    let mut out: Vec<u8> = Vec::with_capacity(skeleton.len() + pairs * 22);
    let (mut lat, mut lng) = (0i64, 0i64);
    let mut taken = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    let mut i = 0;
    while i < skeleton.len() {
        let b = skeleton[i];
        if in_string {
            out.push(b);
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if b == b'"' {
            in_string = true;
            out.push(b'"');
            i += 1;
            continue;
        }
        if skeleton[i..].starts_with(TOKEN) && !follows_digit(&skeleton[i + TOKEN.len()..]) {
            if taken == pairs {
                return None;
            }
            lat += r.zigzag()?;
            lng += r.zigzag()?;
            taken += 1;
            let mut pair = String::with_capacity(24);
            pair.push('[');
            push_decimal(&mut pair, lat);
            pair.push(',');
            push_decimal(&mut pair, lng);
            pair.push(']');
            out.extend_from_slice(pair.as_bytes());
            i += TOKEN.len();
            continue;
        }
        out.push(b);
        i += 1;
    }
    if taken != pairs || in_string {
        return None;
    }
    // Bytes were copied as bytes, so a multi-byte character in a string
    // (Curaçaostraat) comes through as itself.
    String::from_utf8(out).ok()
}

/// `-0.0` followed by a digit would be `-0.01`, a real number.
fn follows_digit(rest: &[u8]) -> bool {
    rest.first().is_some_and(u8::is_ascii_digit)
}

/// Walk a value, replacing every packable pair by the token and appending its
/// units to `pairs` in document order (object keys in their sorted order, as the
/// serialiser will emit them).
fn take_pairs(value: Value, pairs: &mut Vec<(i64, i64)>) -> Value {
    match value {
        Value::Array(items) => {
            if let [Value::Number(a), Value::Number(b)] = items.as_slice() {
                if let (Some(a), Some(b)) = (units(a), units(b)) {
                    pairs.push((a, b));
                    return Value::from(-0.0f64);
                }
            }
            Value::Array(items.into_iter().map(|v| take_pairs(v, pairs)).collect())
        }
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(k, v)| (k, take_pairs(v, pairs)))
                .collect(),
        ),
        other => other,
    }
}

/// A decimal number as 10⁻⁷ units, when it is one with at most seven decimals
/// that comes back as the same double. An integer-looking number (`96`) is left
/// alone: it may be a count, and a count is not a coordinate.
fn units(n: &serde_json::Number) -> Option<i64> {
    if n.is_i64() || n.is_u64() {
        return None;
    }
    let v = n.as_f64()?;
    if !v.is_finite() || v.abs() > 1e6 {
        return None;
    }
    let scaled = v * UNIT;
    let u = scaled.round();
    if (scaled - u).abs() > 1e-3 {
        return None;
    }
    let u = u as i64;
    let mut text = String::new();
    push_decimal(&mut text, u);
    (text.parse::<f64>().ok()? == v).then_some(u)
}

/// `u` units as the shortest decimal with at most seven places — `532347567` →
/// `53.2347567`, `65500000` → `6.55`, `0` → `0.0`.
fn push_decimal(out: &mut String, u: i64) {
    if u < 0 {
        out.push('-');
    }
    let u = u.unsigned_abs();
    let whole = u / 10_000_000;
    let mut frac = u % 10_000_000;
    out.push_str(&whole.to_string());
    out.push('.');
    if frac == 0 {
        out.push('0');
        return;
    }
    let mut digits = 7;
    while frac % 10 == 0 {
        frac /= 10;
        digits -= 1;
    }
    let s = frac.to_string();
    for _ in s.len()..digits {
        out.push('0');
    }
    out.push_str(&s);
}

/// Tokens outside string literals.
fn count_tokens(text: &[u8]) -> usize {
    let (mut n, mut in_string, mut escaped, mut i) = (0, false, false, 0);
    while i < text.len() {
        let b = text[i];
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            i += 1;
        } else if b == b'"' {
            in_string = true;
            i += 1;
        } else if text[i..].starts_with(TOKEN) && !follows_digit(&text[i + TOKEN.len()..]) {
            n += 1;
            i += TOKEN.len();
        } else {
            i += 1;
        }
    }
    n
}

fn varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn zigzag(out: &mut Vec<u8>, v: i64) {
    varint(out, ((v << 1) ^ (v >> 63)) as u64);
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn varint(&mut self) -> Option<u64> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let b = *self.bytes.get(self.at)?;
            self.at += 1;
            v |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return Some(v);
            }
        }
        None
    }
    fn zigzag(&mut self) -> Option<i64> {
        let v = self.varint()?;
        Some(((v >> 1) as i64) ^ -((v & 1) as i64))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = r#"{"streets":["Curaçaostraat","Oosterstraat"],"buildings":[{"area":96,"doors":[[53.2348191,6.5568367]],"fp":[[53.2347567,6.5569274],[53.2348552,6.5568762],[53.231024,6.5594703],[53.2,6.5]],"id":"0014100010903947","nr":"12-0.0","units":1,"use":1,"year":1964}],"h":[1.5,-0.25],"n":[0,1],"s":"a\"b\\"}"#;

    #[test]
    fn a_document_comes_back_as_the_same_values() {
        let packed = pack(DOC).expect("packs");
        assert!(looks_packed(&packed));
        let back = unpack(&packed).expect("unpacks");
        let a: Value = serde_json::from_str(DOC).unwrap();
        let b: Value = serde_json::from_str(&back).unwrap();
        assert_eq!(a, b);
        // Five coordinate pairs and the two-float `h` go out as deltas; the
        // integer pair `n` stays text, and so does the string that spells the
        // token.
        assert!(back.contains(r#""n":[0,1]"#));
        assert!(back.contains(r#""nr":"12-0.0""#));
        assert!(back.contains("[53.2,6.5]"));
        assert!(packed.len() < DOC.len());
    }

    #[test]
    fn decimals_are_shortest() {
        for (u, s) in [
            (532_347_567, "53.2347567"),
            (65_500_000, "6.55"),
            (0, "0.0"),
            (-2_500_000, "-0.25"),
            (10_000_000, "1.0"),
            (1, "0.0000001"),
        ] {
            let mut out = String::new();
            push_decimal(&mut out, u);
            assert_eq!(out, s);
        }
    }

    #[test]
    fn what_is_not_packable_is_declined_or_left() {
        assert_eq!(pack("not json"), None);
        assert_eq!(
            pack(r#"{"a":[1,2]}"#),
            None,
            "no decimal pairs: nothing to take out"
        );
        // A number with more than seven decimals stays text.
        let back = unpack(&pack(r#"{"p":[[1.123456789,2.5]],"q":[3.5,4.5]}"#).unwrap()).unwrap();
        assert!(back.contains("1.123456789"));
        // A document that already holds the token is served as JSON only.
        assert_eq!(pack(r#"{"z":-0.0,"p":[[1.5,2.5]]}"#), None);
        assert_eq!(unpack(b"CPJ1"), None);
        assert_eq!(unpack(b"{\"json\":true}"), None);
        assert_eq!(
            packed_path("bag/14/1/2.places.json"),
            Some("bag/14/1/2.places.pj".into())
        );
        assert_eq!(packed_path("ground/14/1/2.bin"), None);
    }
}
