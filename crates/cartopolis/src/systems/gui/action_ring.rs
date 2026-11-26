//! The ring of actions: round buttons around the thing that was tapped, the
//! primary one in brand orange at the top, ✕ in the middle where the thumb
//! already is, and a pill underneath saying what was tapped
//! (`docs/mockups/layers-and-ring/v1.png`, drawn first for people in chat-world
//! v4). One widget for every subject — a point on the map, an object, a
//! satellite — so the gesture means the same thing wherever it lands.
//!
//! The component library draws it (`lib_action_ring`). This module only
//! decides: the caller says which actions exist, [`run`] says where the ring
//! stands and whether a press off it or Escape closes it, publishes that as a
//! [`RingView`], and hands back what was tapped on the library's ring
//! ([`RingLib::acts`]).

use std::hash::{Hash, Hasher};

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;

use crate::systems::ui_input::UiMetrics;

/// One button on the ring.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RingAction {
    /// What the caller matches on when it is pressed.
    pub key: &'static str,
    pub icon: &'static str,
    /// Short — it is set under the icon inside a 56 pt disc.
    pub label: &'static str,
    /// Hover text, for the desktop.
    pub hint: &'static str,
}

/// What happened to the ring this frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RingOutcome {
    #[default]
    Open,
    /// A button was pressed: its key. The caller closes the ring.
    Chosen(&'static str),
    /// ✕, Escape, or a press anywhere off the ring.
    Dismissed,
}

/// Which ring a view and its taps belong to: new for each opening, so the
/// press that opened one is not read as a press off it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RingId(pub u64);

impl RingId {
    /// The id of `what` (a name and whatever makes this opening new).
    #[must_use]
    pub fn new(what: impl Hash) -> Self {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        what.hash(&mut h);
        Self(h.finish())
    }
}

/// Radius from the centre to each button's centre, points.
pub(crate) const RADIUS: f32 = 78.0;
/// A button's radius, points: 56 pt across, above the 48 dp touch floor.
pub(crate) const BUTTON_R: f32 = 28.0;
/// The centre ✕'s radius.
pub(crate) const CLOSE_R: f32 = 17.0;
/// How far under the buttons the pill starts, points.
pub(crate) const PILL_GAP: f32 = 8.0;

/// One open ring, as the component library draws it: everything but the
/// painting. `centre` is already pulled on screen ([`clamp_centre`]).
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub struct RingView {
    /// The ring's id: new for each opening, and the tag its taps come back
    /// with.
    pub id: RingId,
    /// Ui points.
    pub centre: Vec2,
    /// The viewport's width, ui points: the pill is centred in a band that
    /// must stay on screen.
    pub screen_w: f32,
    pub title: String,
    pub detail: String,
    pub actions: Vec<RingAction>,
}

/// The bridge between a ring's caller and the library's ring
/// (`lib_action_ring`): what it draws, and what was tapped on it.
#[derive(Debug, Default)]
pub struct RingLib {
    /// The ring open this frame, `None` when there is none.
    pub view: Option<RingView>,
    /// Taps on the library's ring since the last frame, with the ring they
    /// were on.
    pub acts: Vec<(RingId, RingOutcome)>,
}

impl RingLib {
    /// No ring this frame: the library takes its down.
    pub fn hide(&mut self) {
        if self.view.is_some() {
            self.view = None;
        }
        if !self.acts.is_empty() {
            self.acts.clear();
        }
    }
}

/// The map's ring (`ui::drive_context_menu`) bridged to the library.
/// Inserted by the library's driver the first frame it runs.
#[derive(Resource, Debug, Default)]
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub struct MapRingLib(pub RingLib);

/// This frame's input as a ring reads it: a press, and where, and Escape.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RingInput {
    /// A pointer button went down this frame, at this point (ui points) —
    /// a finger too, which `touch` emulates as the mouse.
    pub press: Option<Vec2>,
    pub escape: bool,
    /// The viewport, ui points.
    pub screen: Vec2,
}

/// What a ring's caller reads its [`RingInput`] from.
#[derive(SystemParam)]
pub struct RingPointer<'w, 's> {
    mouse: Option<Res<'w, ButtonInput<MouseButton>>>,
    keys: Option<Res<'w, ButtonInput<KeyCode>>>,
    windows: Query<'w, 's, &'static Window, With<PrimaryWindow>>,
    metrics: Res<'w, UiMetrics>,
}

impl RingPointer<'_, '_> {
    /// This frame's [`RingInput`].
    #[must_use]
    pub fn read(&self) -> RingInput {
        let pressed = self.mouse.as_ref().is_some_and(|m| {
            m.any_just_pressed([MouseButton::Left, MouseButton::Right, MouseButton::Middle])
        });
        let cursor = self
            .windows
            .iter()
            .next()
            .and_then(Window::cursor_position)
            .map(|p| self.metrics.window_to_ui(p));
        RingInput {
            press: if pressed { cursor } else { None },
            escape: self
                .keys
                .as_ref()
                .is_some_and(|k| k.just_pressed(KeyCode::Escape)),
            screen: self.metrics.viewport_pts(),
        }
    }

    /// A point in the window's logical points, in ui points.
    #[must_use]
    pub fn window_to_ui(&self, p: Vec2) -> Vec2 {
        self.metrics.window_to_ui(p)
    }
}

/// Where the `i`th of `n` buttons sits: evenly round the circle, the first (the
/// primary) straight up.
#[must_use]
pub fn slot(i: usize, n: usize) -> Vec2 {
    let a = -std::f32::consts::FRAC_PI_2 + std::f32::consts::TAU * i as f32 / n.max(1) as f32;
    Vec2::new(a.cos(), a.sin()) * RADIUS
}

/// The ring's centre, pulled in from the edges of a `screen` (ui points, from
/// the origin) so every button and the pill stay on screen.
#[must_use]
pub fn clamp_centre(at: Vec2, screen: Vec2) -> Vec2 {
    let margin = RADIUS + BUTTON_R + 6.0;
    let below = RADIUS + BUTTON_R + 54.0;
    Vec2::new(
        at.x.clamp(margin, (screen.x - margin).max(margin)),
        at.y.clamp(margin, (screen.y - below).max(margin)),
    )
}

/// The ring around `at` (ui points): published in `lib.view` for the
/// library to draw, and what was tapped on it — or a press off it, or Escape,
/// from `input` — is the outcome. `id` must be stable for as long as this
/// ring is open, and new for a new one: the press that opened it is still
/// this frame's, and is not a press "off the ring".
pub fn run(
    lib: &mut RingLib,
    input: RingInput,
    id: RingId,
    at: Vec2,
    title: &str,
    detail: &str,
    actions: &[RingAction],
) -> RingOutcome {
    let centre = clamp_centre(at, input.screen);
    // The frame it opens on: the view is not yet this ring's.
    let first = lib.view.as_ref().is_none_or(|v| v.id != id);
    let fresh = lib.view.as_ref().is_none_or(|v| {
        v.id != id
            || v.centre != centre
            || v.screen_w != input.screen.x
            || v.title != title
            || v.detail != detail
            || v.actions != actions
    });
    if fresh {
        lib.view = Some(RingView {
            id,
            centre,
            screen_w: input.screen.x,
            title: title.to_owned(),
            detail: detail.to_owned(),
            actions: actions.to_vec(),
        });
    }
    let mut outcome = lib
        .acts
        .drain(..)
        .filter(|(on, _)| *on == id)
        .map(|(_, o)| o)
        .next_back()
        .unwrap_or_default();
    if outcome == RingOutcome::Open && dismissed(input, centre, first) {
        outcome = RingOutcome::Dismissed;
    }
    if outcome != RingOutcome::Open {
        lib.view = None;
    }
    outcome
}

/// Whether Escape, or a press off the ring around `c` after the frame it
/// opened, closes it.
fn dismissed(input: RingInput, c: Vec2, first: bool) -> bool {
    let off_ring = input
        .press
        .is_some_and(|p| p.distance(c) > RADIUS + BUTTON_R + 4.0);
    input.escape || (off_ring && !first)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_primary_is_on_top_and_the_rest_go_round() {
        let top = slot(0, 5);
        assert!(top.x.abs() < 1e-3 && top.y < 0.0, "{top:?}");
        // Evenly spaced: every neighbour is the same chord apart.
        let chord = (slot(1, 5) - slot(0, 5)).length();
        for i in 1..5 {
            let d = (slot((i + 1) % 5, 5) - slot(i, 5)).length();
            assert!((d - chord).abs() < 1e-3);
        }
    }

    #[test]
    fn the_ring_is_pulled_on_screen() {
        let screen = Vec2::new(400.0, 800.0);
        let c = clamp_centre(Vec2::new(2.0, 790.0), screen);
        assert!(c.x - RADIUS - BUTTON_R >= 0.0);
        assert!(c.y + RADIUS + BUTTON_R + 40.0 <= 800.0);
    }

    #[test]
    fn the_opening_press_does_not_close_the_ring_but_a_later_one_does() {
        let screen = Vec2::new(800.0, 800.0);
        let mut lib = RingLib::default();
        let id = RingId::new(("t", 1));
        let at = Vec2::new(400.0, 400.0);
        let off = RingInput {
            press: Some(Vec2::new(10.0, 10.0)),
            escape: false,
            screen,
        };
        assert_eq!(run(&mut lib, off, id, at, "a", "", &[]), RingOutcome::Open);
        assert_eq!(
            run(&mut lib, off, id, at, "a", "", &[]),
            RingOutcome::Dismissed
        );
        assert!(lib.view.is_none());
    }
}
