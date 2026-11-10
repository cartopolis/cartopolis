//! Which of the two palettes the chrome wears ([`theme::DARK`], [`theme::LIGHT`]).
//!
//! **The UI follows the light.** The app already has a light switch — Auto ·
//! Light · Dark under the locate disc (`hud`), `SunMode` underneath — and the
//! chrome takes its answer from it rather than growing a second switch that
//! could disagree: Light is the light theme, Dark the dark one, and Auto (or a
//! time set in Settings ▸ World) is light while the sun is up where the map is
//! and dark once it has set.
//!
//! `CARTO_UI_THEME=light|dark` (`?ui_theme=light` on the web) forces one, so a
//! headless capture can photograph either whatever the hour.

use bevy::prelude::*;

use crate::resources::{DevPanelState, SunMode};
use crate::systems::egui_manager::EguiRef;
use crate::systems::theme;

/// A forced theme from `CARTO_UI_THEME`: `Some(true)` light, `Some(false)` dark.
fn forced() -> Option<bool> {
    static FORCED: std::sync::OnceLock<Option<bool>> = std::sync::OnceLock::new();
    *FORCED.get_or_init(|| {
        let v = crate::env_value("CARTO_UI_THEME")?;
        match v.trim().to_ascii_lowercase().as_str() {
            "light" => Some(true),
            "dark" => Some(false),
            other => {
                warn!(value = %other, "CARTO_UI_THEME is light or dark; following the light");
                None
            }
        }
    })
}

/// Light or dark for a sun mode, the hour it holds and the season.
fn wants_light(mode: SunMode, time_of_day: f32, declination_rad: f32) -> bool {
    match mode {
        SunMode::Day => true,
        SunMode::Night => false,
        // The real sun (Auto) or a set hour (Manual): light while it is up.
        SunMode::Auto | SunMode::Manual => {
            crate::utils::sun::sun_direction(time_of_day, declination_rad).y > 0.0
        }
    }
}

/// Set the palette and re-apply egui's visuals, only on the frame the answer
/// changes (and the first).
pub fn follow_the_light(dev: Res<DevPanelState>, egui: EguiRef, mut was: Local<Option<bool>>) {
    let light = forced()
        .unwrap_or_else(|| wants_light(dev.sun_mode, dev.time_of_day, dev.sun_declination_rad));
    if *was == Some(light) {
        return;
    }
    *was = Some(light);
    theme::set_light(light);
    theme::apply_visuals(egui.ctx());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn light_and_dark_are_the_theme_and_auto_follows_the_sun() {
        let decl = 0.0;
        assert!(wants_light(SunMode::Day, 0.0, decl));
        assert!(!wants_light(SunMode::Night, 12.0, decl));
        assert!(wants_light(SunMode::Auto, 12.0, decl), "noon is light");
        assert!(!wants_light(SunMode::Auto, 0.0, decl), "midnight is dark");
        assert!(
            !wants_light(SunMode::Manual, 23.0, decl),
            "a set late hour is dark"
        );
    }
}
