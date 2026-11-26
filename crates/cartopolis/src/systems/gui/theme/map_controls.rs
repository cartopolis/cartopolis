use super::*;

// ─── The map's floating controls ──────────────────────────────────────────────
//
// Everything that floats over the map — the dock, the compass column, the
// on-screen navigation controls, the scale bar — is laid out from these three
// numbers. They used to be per-module constants, and what that bought was six
// edge insets (8, 10, 12, 14, 16 and a bare `-10`) within one screen. The point
// of a floating control is that it reads as *chrome over a map* rather than as
// part of the map, and that only works if there is one of it.

/// Inset of a floating control from the window edge, in points. One number for
/// all four edges: a top-left column indented 12 and a top margin of 8 is a
/// corner that looks like a mistake.
pub const OSD_MARGIN: f32 = 12.0;
/// Gap between two controls that belong to one cluster (the two halves of a
/// zoom pair, the compass and the locate button under it).
pub const OSD_GAP: f32 = 8.0;
/// Gap between two clusters sharing an edge. Deliberately more than twice
/// [`OSD_GAP`]: with one gap for both, a column of discs reads as a single
/// undifferentiated strip, which is what the layers disc and the compass under
/// it had become.
pub const OSD_GROUP_GAP: f32 = 18.0;

/// Midas's gold: the hub's theme and its icon (midas `web/src/icon.svg`), which
/// is [`Palette::warn`]'s value because both are that theme's warning colour —
/// in the light theme too, where the hub's warning is an amber-brown that
/// reads on the light disc the mark sits on.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))] // the link is native-only
#[must_use]
pub fn midas_gold() -> Rgba8 {
    p().warn
}
