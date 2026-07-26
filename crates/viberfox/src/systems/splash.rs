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

/// Baked in at compile time; see the module note on why this is not an asset.
const LOGO_PNG: &[u8] = include_bytes!("../../../../assets/icon/viberfox.png");
/// The wordmark face (SIL OFL — see `assets/fonts/README.md`). Embedded for the
/// same reason as the logo: the splash draws on frame one, before any asset
/// pipeline has run. egui reads TTF/OTF, not woff2, which is why this is a TTF.
const WORDMARK_TTF: &[u8] =
    include_bytes!("../../../../assets/fonts/PlusJakartaSans-SemiBold.ttf");
/// egui font family name for [`WORDMARK_TTF`].
const WORDMARK_FAMILY: &str = "jakarta";

/// Shortest time the splash stays up. Without a floor, a warm cache dismisses it
/// within a frame or two and it reads as a flash of noise rather than a screen.
const MIN_VISIBLE_S: f32 = 1.4;
/// Hard cap. If the world never becomes ready — no network, a failed tile host —
/// the splash must still get out of the way rather than hiding the app forever.
const MAX_VISIBLE_S: f32 = 12.0;
/// Fade duration once dismissal is decided.
const FADE_S: f32 = 0.5;

#[derive(Resource, Default)]
pub struct BootSplash {
    texture: Option<egui::TextureHandle>,
    elapsed: f32,
    /// Counts down through [`FADE_S`] once dismissal begins; `None` until then.
    fading: Option<f32>,
    finished: bool,
    fonts_ready: bool,
}

impl BootSplash {
    /// True once the splash has fully gone. Cheap for other systems to gate on.
    pub fn finished(&self) -> bool {
        self.finished
    }
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

    let ctx = egui.ctx_mut();
    if !splash.fonts_ready {
        // Registered as its *own* family rather than pushed onto Proportional,
        // so the wordmark gets Jakarta and every other panel in the app keeps
        // egui's default face. `FontDefinitions::default()` is safe to start
        // from because nothing else installs custom fonts.
        let mut fonts = egui::FontDefinitions::default();
        fonts.font_data.insert(
            WORDMARK_FAMILY.to_owned(),
            std::sync::Arc::new(egui::FontData::from_static(WORDMARK_TTF)),
        );
        fonts.families.insert(
            egui::FontFamily::Name(WORDMARK_FAMILY.into()),
            vec![WORDMARK_FAMILY.to_owned()],
        );
        ctx.set_fonts(fonts);
        splash.fonts_ready = true;
    }
    if splash.texture.is_none() {
        if let Ok(img) = image::load_from_memory(LOGO_PNG) {
            let img = img.to_rgba8();
            let size = [img.width() as usize, img.height() as usize];
            let color = egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw());
            splash.texture = Some(ctx.load_texture("boot_logo", color, egui::TextureOptions::LINEAR));
        }
    }

    // A foreground layer painted directly, rather than a window or a panel: this
    // covers everything, takes no input, and needs no layout.
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("boot_splash"),
    ));
    // egui 0.35 exposes no `screen_rect` accessor — the app feeds it in through
    // `RawInput` — so take the size from the window itself, in the same logical
    // points egui works in.
    let Ok(win) = window.single() else { return };
    let rect = egui::Rect::from_min_size(
        egui::pos2(0.0, 0.0),
        egui::vec2(win.width(), win.height()),
    );
    let a = |v: f32| (v * alpha * 255.0) as u8;

    // Matches index.html's radial gradient closely enough to read as continuous;
    // egui has no gradient primitive, so this is the gradient's middle tone.
    painter.rect_filled(rect, 0.0, egui::Color32::from_rgba_unmultiplied(13, 17, 20, a(1.0)));

    let c = rect.center();
    if let Some(tex) = &splash.texture {
        let s = 168.0;
        let logo = egui::Rect::from_center_size(
            egui::pos2(c.x, c.y - 104.0),
            egui::vec2(s, s),
        );
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
        egui::FontId::new(40.0, egui::FontFamily::Name(WORDMARK_FAMILY.into())),
        egui::Color32::from_rgba_unmultiplied(232, 230, 225, a(1.0)),
    );
    painter.text(
        egui::pos2(c.x, c.y + 66.0),
        egui::Align2::CENTER_CENTER,
        "Explore our world",
        egui::FontId::proportional(16.0),
        egui::Color32::from_rgba_unmultiplied(159, 179, 191, a(0.9)),
    );
}
