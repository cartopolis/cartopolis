use super::*;

// ─── Shared frames ────────────────────────────────────────────────────────────

/// Padding inside a label chip over the map ([`LABEL_CHIP`] behind a few
/// words, [`RADIUS_SM`] round): the driver's chip and the lens's marks
/// (`lib_driver`, `lib_scenario_map`) use the same two numbers.
pub const CHIP_PADDING: bevy::math::Vec2 = bevy::math::Vec2::new(6.0, 3.0);

/// Height of a primary button on a pointer. Under a finger it grows to
/// [`row_height`], which is larger.
pub const PRIMARY_BUTTON_HEIGHT: f32 = 40.0;

/// The height of a control: the web's `--m-control-height`, 38 px on a
/// pointer and 44 under a finger, which here is the primary button's height
/// or the touch row, whichever is taller (40 / 40 today). Read by the
/// library's probe card (`ui_probe`), native only.
#[cfg_attr(
    not(all(feature = "ui-probe", not(target_arch = "wasm32"))),
    allow(dead_code)
)]
#[must_use]
pub fn control_height() -> f32 {
    PRIMARY_BUTTON_HEIGHT.max(row_height())
}

/// Diameter of a round icon button, in points. Matches the compass disc in the
/// opposite corner — the map's floating controls are one family. Already 60 dp on
/// the touch tier, so it needs no finger-sized variant.
pub const ROUND_BUTTON_SIZE: f32 = 40.0;

/// Hang-up red: the one control in the app that is red at rest, because
/// ending a call is the action every phone draws in it, and a person reaching
/// for it in a hurry looks for the colour before the shape.
pub const CALL_END_RED: Rgba8 = Rgba8::from_rgb(0xD6, 0x45, 0x45);

/// Answer green: the other half of [`CALL_END_RED`], on the one screen where
/// both appear — a call coming in.
pub const CALL_ACCEPT_GREEN: Rgba8 = Rgba8::from_rgb(0x2E, 0xA0, 0x5A);

/// How a round call control is dressed (`lib_call`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CallTone {
    /// A switch that is on (the microphone, the camera): a frosted plate that
    /// reads over a picture as well as over the dark call screen.
    On,
    /// A switch that is *off* wears the solid cream plate: that is the state
    /// worth noticing, and the icon says which (`MIC_OFF`, `VIDEOCAM_OFF`).
    Off,
    /// Hang up, or decline: [`CALL_END_RED`].
    HangUp,
    /// Answer: [`CALL_ACCEPT_GREEN`].
    Accept,
}

/// The call screen's big buttons: a thumb's width, as on a phone's call screen.
pub const CALL_BUTTON_LARGE: f32 = 64.0;
/// The bar a minimised call or a ringing banner shows.
pub const CALL_BUTTON_SMALL: f32 = 36.0;
