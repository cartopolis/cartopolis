//! Which of the two palettes the chrome wears ([`theme::DARK`], [`theme::LIGHT`]).
//!
//! **Its own setting** (`AppPrefs::ui_theme`, Settings ▸ General ▸
//! Appearance, since 2026-11-10): Auto is light while the real sun is up where
//! the map is and dark once it has set; Light and Dark are fixed. Until then the
//! chrome took its answer from the scene's light (`SunMode`, the light disc
//! under the compass), so choosing Dark for the UI also made the map night,
//! and a night sky could not be looked at in a light UI. The scene's light now
//! changes only the sky.
//!
//! `CARTO_UI_THEME=light|dark` (`?ui_theme=light` on the web) forces one, so a
//! headless capture can photograph either whatever the hour.

use bevy::prelude::*;

use crate::resources::{AppPrefs, DevPanelState, UiTheme};
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
                warn!(value = %other, "CARTO_UI_THEME is light or dark; using the setting");
                None
            }
        }
    })
}

/// Light or dark for the setting, the real solar hour at the map and the
/// season.
fn wants_light(theme: UiTheme, solar_hour: f32, declination_rad: f32) -> bool {
    match theme {
        UiTheme::Light => true,
        UiTheme::Dark => false,
        UiTheme::Auto => crate::utils::sun::sun_direction(solar_hour, declination_rad).y > 0.0,
    }
}

/// Set the palette and re-apply egui's visuals, only on the frame the answer
/// changes (and the first).
pub fn follow_the_light(
    prefs: Res<AppPrefs>,
    dev: Res<DevPanelState>,
    clock: Res<crate::systems::transit_live::WorldClock>,
    globe: Res<crate::systems::globe::GlobeState>,
    egui: EguiRef,
    mut was: Local<Option<bool>>,
) {
    let light = forced().unwrap_or_else(|| {
        // The real sun, whatever the scene's light is set to: a sky forced to
        // night is a picture, not the hour.
        let solar = crate::utils::sun::solar_time_of_day(clock.now_unix, globe.anchor_lng);
        wants_light(prefs.ui_theme, solar, dev.sun_declination_rad)
    });
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
    fn light_and_dark_are_fixed_and_auto_follows_the_sun() {
        let decl = 0.0;
        assert!(wants_light(UiTheme::Light, 0.0, decl), "Light at midnight");
        assert!(!wants_light(UiTheme::Dark, 12.0, decl), "Dark at noon");
        assert!(wants_light(UiTheme::Auto, 12.0, decl), "noon is light");
        assert!(!wants_light(UiTheme::Auto, 0.0, decl), "midnight is dark");
    }

    /// A settings file from before the setting, or with a name this build has
    /// never heard of, loads as Auto rather than failing the file.
    #[test]
    fn the_setting_reads_leniently() {
        let old: AppPrefs = serde_json::from_str("{}").expect("an old file loads");
        assert_eq!(old.ui_theme, UiTheme::Auto);
        let dark: AppPrefs = serde_json::from_str(r#"{"ui_theme":"dark"}"#).unwrap();
        assert_eq!(dark.ui_theme, UiTheme::Dark);
        let odd: AppPrefs = serde_json::from_str(r#"{"ui_theme":"sepia"}"#).unwrap();
        assert_eq!(odd.ui_theme, UiTheme::Auto);
    }
}
