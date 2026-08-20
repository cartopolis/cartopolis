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
//! The progress track and the status line under it belong to that parity too, and
//! until 2026-08-20 only the HTML side had them — so the handover went from a
//! screen that said what it was doing to one that said nothing, and a *native*
//! launch never said anything at all. See [`Loading`] for what this half counts,
//! and for the eighteen-second hold that made saying so necessary.
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
use big_space::prelude::GridCell;

use crate::components::{FreeCamera, StreamTileReady};
use crate::systems::egui_manager::EguiManager;
use crate::systems::globe::GlobeState;
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

/// Height of the progress track, in points (`index.html`'s `#track` is 3 px).
const TRACK_H: f32 = 3.0;
/// The status line under the track. Smaller than [`theme::SPLASH_MOTTO`] on
/// purpose: the motto is the brand and this is a caption under it.
const STATUS_TEXT: f32 = 13.0;
/// One 35 %-wide sweep of the indeterminate bar, in seconds. `index.html`'s
/// `@keyframes sweep` runs at 1.1 s and the two splashes are one screen.
const SWEEP_S: f32 = 1.1;
/// Fraction of the track the indeterminate segment occupies (`#bar.indeterminate`).
const SWEEP_FRAC: f32 = 0.35;

/// What the splash is waiting for, in the words under the track.
///
/// **This exists because an eighteen-second hold looked like a crash.** The only
/// thing that dismisses the splash early is the map becoming visible, and above
/// [`map_stream::MAP_PREFETCH_ALT`](crate::systems::map_stream) the flat streamer
/// is *deliberately* frozen — so a launch that restores a camera in orbit (a
/// bookmark taken from space, or simply where the last session left off) had
/// nothing to wait for and sat out [`MAX_VISIBLE_S`] in full, every time, with a
/// static logo and no way for anyone to tell that from a hang. Two things follow
/// and they are separate: [`ready`](Self::ready) now asks whichever streamer is
/// actually feeding the screen, and the label says which one and how far it has
/// got.
struct Loading {
    /// The status line. Owned rather than `&'static str` because the counts are
    /// the diagnostic half — "Streaming the map… 6/224" is what says whether a
    /// long hold is a slow network or a streamer that never started.
    label: String,
    /// Track fill, or `None` for a stage with no honest denominator — drawn as
    /// the sweeping segment rather than as a number nobody measured.
    fraction: Option<f32>,
    /// Enough of the world is on screen that lifting the splash uncovers a map
    /// rather than a void.
    ready: bool,
}

/// Which streamer the splash is waiting on, and how far along it is.
///
/// `tiles_ready` is counted from [`StreamTileReady`] rather than taken from
/// `MapStream` — see [`MapStream::wanted_tiles`] for why the streamer's own
/// `loaded` count cannot be the numerator (it reaches the full ring within a
/// frame or two of an anchor, while every tile is still blank paper).
fn loading(stream: &MapStream, globe: &GlobeState, in_space: bool, tiles_ready: usize) -> Loading {
    if stream.geo_anchor().is_none() {
        return Loading {
            label: "Placing the map…".to_owned(),
            fraction: None,
            ready: false,
        };
    }
    // In space the flat clipmap is frozen *and* hidden; the globe's quadtree is
    // the layer that will be uncovered, so it is the one to wait for and to
    // report. Asking `MapStream` here is what produced the eighteen-second hold.
    let (what, have, want, ready) = if in_space {
        let (have, want) = globe.tile_progress();
        ("globe", have, want, have > 0)
    } else {
        (
            "map",
            tiles_ready,
            stream.wanted_tiles(),
            // Unchanged from the signal this system has always dismissed on: a
            // spawned ring is a paper-coloured map, which is a world. Holding out
            // for textures instead would make every warm start *longer*, and the
            // complaint about this screen has never been that it is too short.
            stream.has_tiles(),
        )
    };
    Loading {
        label: if want > 0 {
            format!("Streaming the {what}… {have}/{want}")
        } else {
            format!("Streaming the {what}…")
        },
        fraction: (want > 0).then(|| (have as f32 / want as f32).clamp(0.0, 1.0)),
        ready,
    }
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
    globe: Res<GlobeState>,
    camera: Query<(&GridCell, &Transform), With<FreeCamera>>,
    tiles_ready: Query<(), With<StreamTileReady>>,
    window: Query<&Window, With<PrimaryWindow>>,
) {
    if splash.finished {
        return;
    }
    let dt = time.delta_secs();
    splash.elapsed += dt;

    // No camera yet is the first frame or two, and "not in space" is the right
    // answer there: the startup restore has not run, so the flat streamer is what
    // is about to fill the screen.
    let in_space = camera
        .single()
        .is_ok_and(|(cell, tf)| globe.in_space(cell, tf));
    let progress = loading(&stream, &globe, in_space, tiles_ready.iter().count());

    // Dismiss on the world being visible, not on a timer alone: fading out into
    // an empty blue void is worse than holding the splash half a second longer.
    // `MAX_VISIBLE_S` is the backstop for when tiles never arrive at all.
    let ready = progress.ready && splash.elapsed >= min_visible_s();
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

    // The track and the line under it — `index.html`'s `#track` / `#bar` /
    // `#status`, in the same order under the same motto and off the same two
    // palette entries (`SURFACE_RAISED` is its `#263842`, `ACCENT` its `#fd6012`).
    // The `min(22rem, 70vw)` is its sizing rule too: on a phone in portrait a
    // fixed 352 pt track runs past both margins.
    let track_w = (viewport.x * 0.7).min(352.0);
    let track = egui::Rect::from_center_size(
        egui::pos2(c.x, c.y + 104.0),
        egui::vec2(track_w, TRACK_H),
    );
    painter.rect_filled(track, TRACK_H * 0.5, fade(theme::SURFACE_RAISED, content, 1.0));
    let bar = match progress.fraction {
        // A measured fill, from the left.
        Some(f) => egui::Rect::from_min_size(track.min, egui::vec2(track_w * f, TRACK_H)),
        // No denominator, so no number: a segment sweeping the track instead. The
        // alternative is a bar sitting at zero, which is what a *stalled* loader
        // looks like — the one reading this screen must not give.
        None => {
            let t = (splash.elapsed / SWEEP_S).fract();
            let x = track.min.x + (t * (1.0 + SWEEP_FRAC) - SWEEP_FRAC) * track_w;
            egui::Rect::from_min_size(
                egui::pos2(x, track.min.y),
                egui::vec2(track_w * SWEEP_FRAC, TRACK_H),
            )
            .intersect(track)
        }
    };
    if bar.width() > 0.0 {
        painter.rect_filled(bar, TRACK_H * 0.5, fade(theme::ACCENT, content, 1.0));
    }
    painter.text(
        egui::pos2(c.x, c.y + 128.0),
        egui::Align2::CENTER_CENTER,
        &progress.label,
        egui::FontId::proportional(STATUS_TEXT),
        fade(theme::TEXT_DIM, content, 0.75),
    );
}
