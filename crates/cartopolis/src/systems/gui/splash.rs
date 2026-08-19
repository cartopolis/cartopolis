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
//! They did **not** match until 2026-08-10: the HTML side was a build harness
//! (a monospace checklist of GPU/asset/worker probes over a scrolling boot log,
//! unconditionally visible) while this side was a brand screen, so every web
//! visitor met the engineer's screen first and the app's own splash second. The
//! loader now paints this screen — logo, [`MOTTO`], the progress track — and the
//! probe list moved behind `?log` and the failure state, which is the only time
//! anyone wants it. The three numbers to keep in step are
//! [`theme::SPLASH_WORDMARK`], [`theme::SPLASH_MOTTO`] and the palette.
//!
//! Native has no such gap, so this is the whole startup screen there.
//!
//! Painted with egui rather than `bevy_ui` for a plain reason: egui is already
//! wired up (`systems::egui_manager`) and ships its own fonts, while `bevy_ui`
//! text would need a font asset the repo does not have. The mark is
//! [`theme::paint_mark`] — a drawing, not an asset — so it is on screen on the
//! very first frame with no decode, no texture upload and no HTTP round trip on
//! the web, and no dependency on the icon PNG having survived Git LFS.

use bevy::prelude::*;
use bevy::window::PrimaryWindow;

use crate::systems::egui_manager::EguiManager;
use crate::systems::map_stream::MapStream;
use crate::systems::theme;

/// The line under the wordmark. Named because `index.html` carries the same
/// words: the two splashes are one screen handed over mid-load, and a motto that
/// changes on the handover is the tell that they are not.
const MOTTO: &str = "Explore our world";

/// Shortest time the splash stays up, in seconds — long enough to be read
/// rather than glimpsed, since a warm cache would otherwise dismiss it within a
/// frame or two and that reads as a flash of noise. The floor even when the
/// world is ready.
///
/// **It is shorter on the web, because the web has already had its splash.**
/// `index.html`'s loader owns the ~70 MB download and holds the same logo,
/// wordmark and motto on screen for several seconds before any of this exists;
/// arriving after it and starting a *second* hold is how a brand screen turns
/// into a wait. Native has no such gap and this is the whole startup screen
/// there — but 3.2 s was a hold on top of a warm start that is often ready in
/// well under one, and a splash you have seen four hundred times is a delay,
/// not an introduction. With [`FADE_S`] the two land at ~2.0 s native and
/// ~1.1 s on the web.
const fn min_visible_s() -> f32 {
    if cfg!(target_arch = "wasm32") {
        0.5
    } else {
        1.4
    }
}
/// Hard cap. If the world never becomes ready — no network, a failed tile host —
/// the splash must still get out of the way rather than hiding the app forever.
const MAX_VISIBLE_S: f32 = 18.0;
/// Fade duration once dismissal is decided. Part of the visible hold, so it is
/// counted in the budget above rather than added to it.
const FADE_S: f32 = 0.6;

/// How long the logo, wordmark and motto take to fade *up* at launch.
///
/// **The backdrop is not part of it, and that is the whole design.**
/// [`theme::SURFACE`] is painted opaque from the first frame, because what is
/// behind this layer is the 3D view with nothing streamed into it yet — fading
/// the panel itself in would open the app on a flash of empty void and then hide
/// it again, which is worse than no transition at all. So the slate is there
/// instantly and the *content* arrives on it.
///
/// Inside [`min_visible_s`] rather than added to it: the splash's total hold is
/// unchanged, and the marks simply spend the first half-second arriving instead
/// of being there already. Shorter than [`FADE_S`] on purpose — an entrance that
/// takes as long as an exit reads as a stutter on a cold start, where the frames
/// are the most expensive ones of the run.
const FADE_IN_S: f32 = 0.45;

/// Smoothstep. Both fades run through it so the entrance and the exit are the
/// same motion in opposite directions; a linear ramp starts and stops abruptly at
/// exactly the two moments anyone is looking at it.
fn ease(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[derive(Resource, Default)]
pub struct BootSplash {
    elapsed: f32,
    /// Counts down through [`FADE_S`] once dismissal begins; `None` until then.
    fading: Option<f32>,
    finished: bool,
}

impl BootSplash {
    /// Still painting over the world. Anything else that wants the screen — the
    /// sign-in card is the one such thing — waits for this, or it draws a second
    /// logo and wordmark on top of the first.
    pub fn covering(&self) -> bool {
        !self.finished
    }
}

/// Draw the splash until the first map tiles are up, then fade it out.
pub fn boot_splash(
    mut splash: ResMut<BootSplash>,
    egui: Res<EguiManager>,
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
    let ready = stream.has_tiles() && splash.elapsed >= min_visible_s();
    if (ready || splash.elapsed >= MAX_VISIBLE_S) && splash.fading.is_none() {
        splash.fading = Some(FADE_S);
    }

    // The dismissal fade, which takes the backdrop with it — this is the layer
    // lifting off the world.
    let backdrop = match splash.fading {
        None => 1.0,
        Some(left) => {
            let left = left - dt;
            splash.fading = Some(left);
            if left <= 0.0 {
                splash.finished = true;
                return;
            }
            ease(left / FADE_S)
        }
    };
    // …and the entrance fade, which the backdrop is deliberately not part of (see
    // `FADE_IN_S`). Multiplied rather than chosen between, so a splash dismissed
    // before it has finished arriving — a warm start with `min_visible_s` at the
    // floor — fades out from wherever it had got to instead of snapping to full.
    let content = backdrop * ease(splash.elapsed / FADE_IN_S);

    // The area to fill, in the points egui lays out in — which are the window's
    // own only while `EguiManager::ui_scale` is 1.0. Reading `Window::width()`
    // here instead (as this did until the touch UI scale existed) oversizes the
    // rect by that factor on a phone, and since everything below is placed
    // relative to its centre, the wordmark walks off the bottom-right corner.
    let viewport = egui.viewport_pts();
    let ctx = egui.ctx();

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
    // Every colour here is a brand constant faded by one of the two alphas above,
    // so the splash and the UI it uncovers are painted from the same palette.
    let fade = |c: egui::Color32, alpha: f32, v: f32| {
        egui::Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), (v * alpha * 255.0) as u8)
    };

    // Matches index.html's radial gradient closely enough to read as continuous;
    // egui has no gradient primitive, so this is the gradient's middle tone.
    painter.rect_filled(rect, 0.0, fade(theme::SURFACE, backdrop, 1.0));

    let c = rect.center();
    // 84 pt of radius — the 168 pt square the logo image used to occupy.
    theme::paint_mark(&painter, egui::pos2(c.x, c.y - 104.0), 84.0, content);
    // Cream `carto`, accent `polis` — the account GUI's split (see
    // `theme::WORDMARK_HEAD`). The brand family is bound because
    // `EguiManager::default` installed the theme before this context's first
    // pass, so there is no unbound-family frame to fall back for
    // (`theme::install` documents why that ordering holds).
    theme::paint_wordmark(
        &painter,
        egui::pos2(c.x, c.y + 24.0),
        theme::SPLASH_WORDMARK,
        content,
    );
    painter.text(
        egui::pos2(c.x, c.y + 66.0),
        egui::Align2::CENTER_CENTER,
        MOTTO,
        egui::FontId::proportional(theme::SPLASH_MOTTO),
        fade(theme::TEXT_DIM, content, 0.9),
    );
}
