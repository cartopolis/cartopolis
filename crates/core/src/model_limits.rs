//! What the model store accepts: the ceilings an uploaded glTF is measured
//! against.
//!
//! # Why these live here and not in the simulator
//!
//! The simulator enforces them (`cartopolis_simulator::assets`, which uses these
//! constants) and is the only thing that decides — a client that skipped a check
//! could not be detected. But three surfaces *say* them before a person uploads
//! anything: the client's Models panel, the atlas web UI's library page, and the
//! simulator's own refusals. They were three hand-written copies of the same four
//! numbers. The two Rust ones now read these constants; the web's TypeScript copy
//! (`UPLOAD_LIMITS` in `crates/atlas/web/src/lib/library.ts`) cannot import Rust,
//! so a test below pins it equal instead.

/// The largest upload accepted, in bytes.
///
/// This is the *read* bound and the first thing applied, before any parsing: a
/// body larger than this is refused without being held whole in memory. Sixteen
/// megabytes is far above any sensible prop — the twenty avatar characters this
/// project ships average about 3 MB each as glTF, and those are rigged, animated
/// characters — and far below anything that threatens the process.
pub const MAX_ASSET_BYTES: usize = 16 * 1024 * 1024;

/// The largest triangle count accepted, summed over every mesh in the file.
///
/// Counted per distinct mesh rather than per node that instances one: what the
/// budget is protecting is GPU memory, and an instanced mesh is uploaded once.
/// Node count is bounded separately, which is the draw-call side of the same
/// question.
pub const MAX_TRIANGLES: u64 = 250_000;

/// The largest node count accepted.
///
/// A model is a prop, not a scene. Thousands of nodes is a level someone has
/// exported whole, and every one of them is a transform to propagate each frame
/// on a phone.
pub const MAX_NODES: usize = 4_000;

/// The largest texture edge accepted, in pixels.
///
/// Read from each image's own header rather than inferred from its compressed
/// size, because the compressed size of a 16384-square image can be tiny — that
/// is what a decompression bomb is. Two thousand and forty-eight is generous for
/// something usually seen from across a street.
pub const MAX_TEXTURE_EDGE: u32 = 2048;

/// [`MAX_ASSET_BYTES`] in whole megabytes, the unit every hint says it in.
pub const MAX_ASSET_MB: usize = MAX_ASSET_BYTES / (1024 * 1024);

/// A count with thousands separators, as the hints print one: `250,000`.
#[must_use]
pub fn with_thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ts_mirror;

    /// The web's upload hint is built from `UPLOAD_LIMITS` in
    /// `crates/atlas/web/src/lib/library.ts`; this is what keeps that copy honest.
    const WEB_LIBRARY: &str = include_str!("../../atlas/web/src/lib/library.ts");
    const WEB_UPLOAD_PAGE: &str =
        include_str!("../../atlas/web/src/components/pages/library/library.tsx");

    #[test]
    fn thousands_are_grouped_the_way_the_hints_print_them() {
        assert_eq!(with_thousands(0), "0");
        assert_eq!(with_thousands(999), "999");
        assert_eq!(with_thousands(2048), "2,048");
        assert_eq!(with_thousands(4_000), "4,000");
        assert_eq!(with_thousands(250_000), "250,000");
        assert_eq!(with_thousands(16_777_216), "16,777,216");
    }

    #[test]
    fn the_web_upload_limits_match_these() {
        let limits = ts_mirror::object_numbers(WEB_LIBRARY, "UPLOAD_LIMITS")
            .expect("`UPLOAD_LIMITS` is missing from atlas/web/src/lib/library.ts");
        let want = [
            ("maxBytes", MAX_ASSET_BYTES as u64),
            ("maxTriangles", MAX_TRIANGLES),
            ("maxNodes", MAX_NODES as u64),
            ("maxTextureEdge", u64::from(MAX_TEXTURE_EDGE)),
        ];
        let mut got_keys: Vec<&str> = limits.iter().map(|(k, _)| k.as_str()).collect();
        got_keys.sort_unstable();
        let mut want_keys: Vec<&str> = want.iter().map(|(k, _)| *k).collect();
        want_keys.sort_unstable();
        assert_eq!(got_keys, want_keys, "UPLOAD_LIMITS names different limits");
        for (key, value) in want {
            let got = limits.iter().find(|(k, _)| k == key).map(|(_, v)| *v);
            assert_eq!(
                got,
                Some(value),
                "UPLOAD_LIMITS.{key} in atlas/web/src/lib/library.ts disagrees with \
                 cartopolis_core::model_limits"
            );
        }
    }

    #[test]
    fn the_web_upload_page_states_the_limits_from_that_copy() {
        // A number typed into the page itself would bypass the pin above.
        assert!(
            WEB_UPLOAD_PAGE.contains("uploadFormatHint()"),
            "the upload page no longer shows `uploadFormatHint()`"
        );
        let hint_fn = WEB_LIBRARY
            .find("function uploadFormatHint")
            .map(|at| &WEB_LIBRARY[at..])
            .expect("`uploadFormatHint` is missing from atlas/web/src/lib/library.ts");
        for key in ["maxBytes", "maxTriangles", "maxNodes", "maxTextureEdge"] {
            assert!(
                hint_fn.contains(&format!("UPLOAD_LIMITS.{key}")),
                "uploadFormatHint no longer reads UPLOAD_LIMITS.{key}"
            );
        }
        for typed in ["250,000", "4,000 nodes", "16 MB", "2048 px"] {
            assert!(
                !WEB_UPLOAD_PAGE.contains(typed),
                "the upload page types `{typed}` by hand; say it from UPLOAD_LIMITS"
            );
        }
    }
}
