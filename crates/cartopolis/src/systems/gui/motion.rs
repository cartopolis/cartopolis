//! Time and curve, in one place — as [`theme`](super::theme) is for colour.
//!
//! Five durations and a curve, the midas hub's own (`web/src/app.css`:
//! `--dur-press` 120 ms, `--dur-state` 200 ms, `--dur-enter` 360 ms,
//! `--ease-out`), so the app and the web UI move alike. A
//! panel never writes a time of its own: it says what kind of change this is
//! (a press, a state, a surface coming or going) and takes the duration from
//! here. `docs/mockups/fluid-ui/` is the design; the second sheet has one
//! rule per layer.
//!
//! Nothing here reads the clock itself: a caller hands in its own elapsed
//! time and takes the duration and the curve from here.
//!
//! **Reduced motion** (`AppPrefs::motion`, Settings ▸ General) zeroes coming
//! and going and keeps press and state: those are feedback, not decoration. It
//! follows the setting and never the frame rate (the rule `effects are
//! settings, not fps`): a phone that stutters gets a faster UI (the third
//! sheet), not a stiller one.

use std::sync::atomic::{AtomicBool, Ordering};

/// A state change: colour and rim of on/off, hover, a tab, a label appearing,
/// the crossfade between the two spaces. Was egui's `Style::animation_time`;
/// kept as the vocabulary's, like [`ENTER`]'s readers.
#[allow(dead_code)]
pub const STATE: f32 = 0.2;
/// A surface arriving: a card, a page, a cover, the keyboard.
pub const ENTER: f32 = 0.36;
/// A surface leaving: two thirds of arriving. Nobody follows what goes; it has
/// to be out of the way before the next tap.
pub const LEAVE: f32 = 0.24;
/// A change nobody asked for — light to dark as the sun sets. (Its reader,
/// the palette crossfade in `ui_theme`, is the design's fifth step.)
#[allow(dead_code)]
pub const AMBIENT: f32 = 0.6;

static REDUCED: AtomicBool = AtomicBool::new(false);

/// Set from the preference once a frame (`apply_pref`).
pub fn set_reduced(on: bool) {
    REDUCED.store(on, Ordering::Relaxed);
}

/// Whether coming and going are switched off.
#[must_use]
pub fn reduced() -> bool {
    REDUCED.load(Ordering::Relaxed)
}

/// [`ENTER`] under the setting. (Its egui readers, the panels' slide, went
/// with egui's panels; kept beside [`ambient_time`] as the vocabulary's.)
#[allow(dead_code)]
#[must_use]
pub fn enter_time() -> f32 {
    if reduced() {
        0.0
    } else {
        ENTER
    }
}

/// [`LEAVE`] under the setting. (As [`enter_time`].)
#[allow(dead_code)]
#[must_use]
pub fn leave_time() -> f32 {
    if reduced() {
        0.0
    } else {
        LEAVE
    }
}

/// [`AMBIENT`] under the setting.
#[allow(dead_code)]
#[must_use]
pub fn ambient_time() -> f32 {
    if reduced() {
        0.0
    } else {
        AMBIENT
    }
}

/// The preference, applied each frame before any panel draws.
pub fn apply_pref(prefs: bevy::prelude::Res<crate::resources::AppPrefs>) {
    set_reduced(prefs.motion == crate::resources::MotionPref::Reduced);
}

/// The hub's `--ease-out`: `cubic-bezier(0.2, 0.8, 0.2, 1)`. After a third of
/// the time 86 % of the way is done, which is why 360 ms does not feel slow.
#[must_use]
pub fn ease_out(t: f32) -> f32 {
    bezier(0.2, 0.8, 0.2, 1.0, t)
}

/// A CSS cubic Bézier timing function: `y` at the `s` where `x(s) == t`, with
/// the endpoints (0, 0) and (1, 1). Newton on `x`, eight steps, which is
/// well under a thousandth for these two curves.
fn bezier(x1: f32, y1: f32, x2: f32, y2: f32, t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t <= 0.0 || t >= 1.0 {
        return t;
    }
    let poly = |a: f32, b: f32, s: f32| {
        let u = 1.0 - s;
        3.0 * u * u * s * a + 3.0 * u * s * s * b + s * s * s
    };
    let slope = |a: f32, b: f32, s: f32| {
        let u = 1.0 - s;
        3.0 * u * u * a + 6.0 * u * s * (b - a) + 3.0 * s * s * (1.0 - b)
    };
    let mut s = t;
    for _ in 0..8 {
        let dx = poly(x1, x2, s) - t;
        let d = slope(x1, x2, s);
        if d.abs() < 1e-6 {
            break;
        }
        s = (s - dx / d).clamp(0.0, 1.0);
    }
    poly(y1, y2, s)
}

/// A sheet's resting heights, lowest first: the title and the one action; the
/// tiles and the first facts; everything, scrolling.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub enum Rest {
    #[default]
    Peek,
    Half,
    Full,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_curves_hit_their_ends_and_the_hubs_numbers() {
        assert_eq!(ease_out(0.0), 0.0);
        assert_eq!(ease_out(1.0), 1.0);
        // After a third of the time, 86 % of the way (the figure the design quotes).
        assert!(
            (ease_out(1.0 / 3.0) - 0.86).abs() < 0.02,
            "{}",
            ease_out(1.0 / 3.0)
        );
        assert!(
            (ease_out(2.0 / 3.0) - 0.98).abs() < 0.02,
            "{}",
            ease_out(2.0 / 3.0)
        );
    }

    #[test]
    fn the_curves_are_monotonic_where_they_should_be() {
        let mut last = 0.0;
        for i in 1..=100 {
            let y = ease_out(i as f32 / 100.0);
            assert!(y >= last - 1e-4, "ease_out dips at {i}: {y} < {last}");
            last = y;
        }
    }

    #[test]
    fn reduced_motion_keeps_state() {
        set_reduced(true);
        assert_eq!(enter_time(), 0.0);
        assert_eq!(leave_time(), 0.0);
        assert_eq!(ambient_time(), 0.0);
        assert_eq!(STATE, 0.2);
        set_reduced(false);
        assert_eq!(enter_time(), ENTER);
    }
}
