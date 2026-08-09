//! Boot splash — the in-app half of the startup screen.
//!
//! **Why there are two of these.** `index.html` carries a splash too, and it is
//! not redundant: on the web the ~70 MB module is fetched and compiled *before
//! any Rust exists*, which is several seconds of black screen this code could
//! not possibly cover. So the HTML one owns "loading the engine" and this one
//! owns "loading the world", and they are drawn to match — same logo, same
//! wordmark, same motto, same background — so the handover reads as one screen
//! rather than two. Change one and change the other.
//!
//! Native has no such gap, so this is the whole startup screen there.
//!
//! Painted with egui rather than `bevy_ui` for a plain reason: egui is already
//! wired up (`systems::egui_manager`) and ships its own fonts, while `bevy_ui`
//! text would need a font asset the repo does not have. The logo is
//! `include_bytes!`-ed rather than loaded through the asset server so it is
//! available on the very first frame — an asset fetch would arrive after the
//! moment it is needed, and on the web would be an HTTP round trip.

use bevy::prelude::*;
use bevy::window::PrimaryWindow;

use crate::systems::egui_manager::EguiManager;
use crate::systems::map_stream::MapStream;
use crate::systems::theme;

/// Baked in at compile time; see the module note on why this is not an asset.
const LOGO_PNG: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/icon/viberfox.png"
));

/// Shortest time the splash stays up. Long enough to be read rather than
/// glimpsed: a warm cache otherwise dismisses it within a frame or two, which
/// reads as a flash of noise. This is the floor even when the world is ready.
const MIN_VISIBLE_S: f32 = 3.2;
/// Hard cap. If the world never becomes ready — no network, a failed tile host —
/// the splash must still get out of the way rather than hiding the app forever.
const MAX_VISIBLE_S: f32 = 18.0;
/// Fade duration once dismissal is decided.
const FADE_S: f32 = 0.9;

#[derive(Resource, Default)]
pub struct BootSplash {
    texture: Option<egui::TextureHandle>,
    elapsed: f32,
    /// Counts down through [`FADE_S`] once dismissal begins; `None` until then.
    fading: Option<f32>,
    finished: bool,
}

/// Draw the splash until the first map tiles are up, then fade it out.
pub fn boot_splash(
    mut splash: ResMut<BootSplash>,
    mut egui: ResMut<EguiManager>,
    time: Res<Time<Real>>,
    stream: Res<MapStream>,
    window: Query<&Window, With<PrimaryWindow>>,
) {
    if splash.finished {
        return;
    }
    let dt = time.delta_secs();
    splash.elapsed += dt;

    // Dismiss on the world being visible, not on a timer alone: fading out into
    // an empty blue void is worse than holding the splash half a second longer.
    // `MAX_VISIBLE_S` is the backstop for when tiles never arrive at all.
    let ready = stream.has_tiles() && splash.elapsed >= MIN_VISIBLE_S;
    if (ready || splash.elapsed >= MAX_VISIBLE_S) && splash.fading.is_none() {
        splash.fading = Some(FADE_S);
    }

    let alpha = match splash.fading {
        None => 1.0,
        Some(left) => {
            let left = left - dt;
            splash.fading = Some(left);
            if left <= 0.0 {
                splash.finished = true;
                splash.texture = None; // release the GPU texture with it
                return;
            }
            left / FADE_S
        }
    };

    // The area to fill, in the points egui lays out in — which are the window's
    // own only while `EguiManager::ui_scale` is 1.0. Reading `Window::width()`
    // here instead (as this did until the touch UI scale existed) oversizes the
    // rect by that factor on a phone, and since everything below is placed
    // relative to its centre, the wordmark walks off the bottom-right corner.
    let viewport = egui.viewport_pts();
    let ctx = egui.ctx_mut();
    if splash.texture.is_none() {
        if let Ok(img) = image::load_from_memory(LOGO_PNG) {
            let img = img.to_rgba8();
            let size = [img.width() as usize, img.height() as usize];
            let color = egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw());
            splash.texture =
                Some(ctx.load_texture("boot_logo", color, egui::TextureOptions::LINEAR));
        }
    }

    // A foreground layer painted directly, rather than a window or a panel: this
    // covers everything, takes no input, and needs no layout.
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("boot_splash"),
    ));
    // The window is still queried, but only as a *gate*: a headless `--shot` run
    // has no primary window, and that is what keeps the splash out of every
    // captured image. The size comes from `viewport_pts` above.
    if window.single().is_err() {
        return;
    }
    let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), viewport);
    let a = |v: f32| (v * alpha * 255.0) as u8;
    // Every colour here is a brand constant faded by the dismissal alpha, so the
    // splash and the UI it uncovers are painted from the same palette.
    let fade =
        |c: egui::Color32, v: f32| egui::Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), a(v));

    // Matches index.html's radial gradient closely enough to read as continuous;
    // egui has no gradient primitive, so this is the gradient's middle tone.
    painter.rect_filled(rect, 0.0, fade(theme::SURFACE, 1.0));

    let c = rect.center();
    if let Some(tex) = &splash.texture {
        let s = 168.0;
        let logo = egui::Rect::from_center_size(egui::pos2(c.x, c.y - 104.0), egui::vec2(s, s));
        painter.image(
            tex.id(),
            logo,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            egui::Color32::from_rgba_unmultiplied(255, 255, 255, a(1.0)),
        );
    }
    painter.text(
        egui::pos2(c.x, c.y + 24.0),
        egui::Align2::CENTER_CENTER,
        "Viberfox",
        // Bound since `EguiManager::default` installed the theme, i.e. before
        // this context's first pass — so there is no unbound-family frame to
        // fall back for (`theme::install` documents why that ordering holds).
        egui::FontId::new(40.0, theme::brand_family()),
        fade(theme::CREAM, 1.0),
    );
    painter.text(
        egui::pos2(c.x, c.y + 66.0),
        egui::Align2::CENTER_CENTER,
        "Explore our world",
        egui::FontId::proportional(16.0),
        fade(theme::TEXT_DIM, 0.9),
    );
}
