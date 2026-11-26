//! The Cartopolis look: one palette, one `egui::Style`, one place to change them.
//!
//! **Deep teal.** A teal-grey ramp — `#0F1416` ink, `#141A1C` surface,
//! `#1F2829` raised, `#2A3536` hover — carries the chrome, and the live things
//! are a clear **teal**: [`Palette::accent`] `#5CC6C0`, with [`Palette::ink`] on it, and its dark
//! half [`Palette::accent_muted`] `#263F42` for a selected row or a pressed button. The
//! values are read off Android's keyboard in its teal dark theme (2026-10-10),
//! which is the look asked for: the surface, the key and the shift-key teal are
//! that keyboard's own. [`Palette::warn`] and [`Palette::error`] are the only other hues, and
//! [`BRAND`] — the logo's orange — stays with the mark, the wordmark and the
//! badges, where against teal it is the logo's original pairing.
//!
//! **How it got here, so nobody walks the same circle.** The ramp was a slate
//! blue with the full brand orange as accent; then (2026-09-10) it went
//! monochrome — a blue-teal grey with a *pale grey* accent — on the argument
//! that a saturated accent competes with a colourful map; then (2026-10-04)
//! the grey was turned toward orange, the panels tinted brown, and finally a
//! neutral charcoal with an orange "ember" accent. The pale grey accent was
//! rejected because every selected tab, ticked box and engaged button read as
//! one more shade of grey — the UI looked dull — and orange mixed *into* the
//! surfaces read as brown. The charcoal and ember was replaced at the user's
//! request for a teal look shared with the web UI (`web_theme`). What survives
//! all of them is the usual rule for a dark UI: **quiet surfaces, one saturated
//! accent, used sparingly**, appearing only where something is selected,
//! engaged, pressed or focused — which keeps it meaning "this is the live
//! thing" over a map. The accent being a *saturated* teal rather than the pale
//! grey of 2026-09 is the difference that matters.
//!
//! Three consequences worth knowing before editing a value:
//!
//!   - **The state ladder is monotonic in lightness** — rest ([`Palette::surface_raised`])
//!     → hover ([`Palette::surface_hover`] plus a [`Palette::accent_word`] rim) → pressed
//!     ([`Palette::accent_muted`], a deep teal, plus the same rim) → engaged
//!     (an [`Palette::accent`] plate carrying [`Palette::on_accent`]). Each step is up, so state still
//!     reads for someone who cannot see the hue.
//!   - **Text on the accent is [`Palette::on_accent`], white** (since 2026-11-10,
//!     on a deep teal; it was dark ink on a bright one). A pressed button keeps
//!     its fill [`Palette::accent_muted`] (see [`widgets`]), so its label stays
//!     [`Palette::cream`].
//!   - **Two constants deliberately share a value.** [`Palette::accent_text`] and
//!     [`Palette::accent_bright`] are both pure white: one is type, one is a stroke, they
//!     answer to different tests. Keeping both names is what lets one move
//!     without the other.
//!
//! **Every surface is solid** (since 2026-11-08, "a bad implementation of
//! glass"): the midas hub's layering, one darkness. [`Palette::panel`], [`Palette::glass`] and
//! [`Palette::overlay`] are all one dark teal, with [`Palette::card`] blocks inside, close to it, and a soft
//! shadow, with the hub's hairline ([`Palette::panel_rim`]). Nothing shows through.
//!
//! Two things this replaces, both of which the UI used to get by default:
//!
//!   - **egui's stock dark theme**, which is a neutral grey (`gray(27)` panels)
//!     with a blue selection. Nothing about it said Cartopolis, and next to the
//!     map — which is bright — flat grey read as a hole rather than a surface.
//!   - **egui's stock spacing**, which is tuned for a debug inspector: 3 pt
//!     between rows and 1 pt of vertical button padding. In a panel of place
//!     rows and layer toggles that is a wall of text; the values here roughly
//!     double the vertical rhythm and give buttons a real hit area.
//!
//! Contrast was checked against the surface each colour actually lands on:
//! [`Palette::text`] is 14.3:1 on [`Palette::surface`], [`Palette::text_dim`] 9.4:1, [`Palette::accent_text`] 17.6:1.
//! Plain [`Palette::accent`] is 3.9:1, enough for a fill, a rule or a ring (3:1) on any
//! surface in the ramp. Anything sitting *on* an accent fill is drawn in
//! [`Palette::on_accent`], white, at 4.5:1.
//! (The numbers in this note are the dark theme's.)
//!
//! **Two palettes, since 2026-11-10.** Everything above describes [`DARK`],
//! which is the look as it was, value for value. [`LIGHT`] is the midas hub's
//! light tokens put in the same roles; the chrome reads the one in force
//! through [`p`], and `systems::ui_theme` picks it from the app's light
//! setting (Light, Dark, or Auto by the sun). The map's own labels, the splash,
//! the call and camera screens and the script editor's code box stay dark in
//! both ([`dark_here`], [`use_dark_visuals`]).
//!
//! [`install`] is called once, from `EguiManager::default`, before the first
//! pass — early enough that the brand font family is bound on frame one, which
//! is what lets the splash paint its wordmark immediately.
//!
//! **Four scales, and nothing outside them.** Colour was the only axis this file
//! originally owned, and the drift went where there was no vocabulary to drift
//! from: the app shipped 17 text sizes and 8 corner radii against one exported
//! radius, because a panel that needed "a bit smaller than body" had nothing to
//! name. So the type scale ([`SIZE_SUB`] and friends), the corner radii
//! ([`RADIUS`], [`RADIUS_SM`], [`RADIUS_LG`], [`RADIUS_PILL`]) and the touch
//! sizes ([`MIN_TOUCH_TARGET_DP`], [`row_height`]) are exported beside the
//! palette. Pick by **meaning**, never by the number — a call site that reads
//! `SIZE_SUB` tells the next person what the text *is*, and `12.5` tells them
//! only what someone once liked.

use egui::{
    style::{Selection, WidgetVisuals, Widgets},
    Color32, CornerRadius, FontFamily, FontId, Margin, Shadow, Stroke, TextStyle,
};

use std::sync::atomic::{AtomicBool, Ordering};

use crate::systems::icons;

mod brand;
mod color;
mod frames;
mod map_controls;
mod metrics;
mod palette;
mod palette32;
mod style;
// Read by the web theme's test and by the library's backends (`ui-lib`);
// in a lane with neither it is the table nothing reads yet.
#[cfg_attr(not(any(test, feature = "ui-lib")), allow(dead_code))]
pub mod tokens;

pub use brand::*;
pub use color::Rgba8;
pub use frames::*;
pub use map_controls::*;
pub use metrics::*;
pub use palette::*;
pub use palette32::*;
pub use style::*;
// The site's tokens (`theme.css`, written by the web theme test) and the
// app's, for the library's screens.
#[cfg(test)]
pub use tokens::tokens;
#[cfg(feature = "ui-lib")]
pub use tokens::{app_tokens, map_vars};

#[cfg(test)]
mod golden;
#[cfg(test)]
mod tests;
