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
use crate::resources::AuthIdentity;
use crate::systems::egui_manager::EguiManager;
use crate::systems::globe::GlobeState;
use crate::systems::map_stream::MapStream;
use crate::systems::session::{SessionMode, SessionState, SessionStatus};
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
/// Fewest paints the dismissal fade may take, whatever the frame rate.
///
/// The fade dismisses into the busiest seconds of the run — the startup stream —
/// and there a frame can take several hundred milliseconds, which spent the
/// whole [`FADE_S`] budget across one or two paints: the math ran and nobody saw
/// it, the splash simply cut to the map.
const MIN_FADE_FRAMES: f32 = 12.0;
/// …and the most wall clock it may spend buying them, as a multiple of
/// [`FADE_S`].
///
/// **A floor on the frame count alone has no ceiling on the wall clock, and that
/// is the other half of the same bug.** Measured 2026-08-26 in a headless launch
/// from 300 km: frames ran 250–800 ms while the globe streamed, so twelve paints
/// took **4.4 seconds** — four seconds of a half-transparent splash sitting over
/// an app that was already running and already taking input. That is the state
/// the screenshot behind this fix was taken in, and it reads as a hang, not as a
/// transition. The fade now advances by at least `1/MAX_FADE_STRETCH` of real
/// time, so it can trade paints for promptness when the frames are expensive:
/// twelve when they are cheap, five at 4 fps, never more than about
/// `FADE_S * MAX_FADE_STRETCH` of anybody's time.
const MAX_FADE_STRETCH: f32 = 2.0;

/// How long the logo, wordmark and motto take to fade *up* at launch.
///
/// **The backdrop is not part of it, and that is the whole design.**
/// [`theme::SURFACE`] is painted opaque from the first frame, because what is
/// behind this layer is the 3D view with nothing streamed into it yet — fading
/// the panel itself in would open the app on a flash of empty void and then hide
/// it again, which is worse than no transition at all. So the backdrop is there
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
/// Below this fill the track is drawn as the sweeping segment instead.
///
/// A determinate bar at zero paints an *empty* track, which is exactly what a
/// stalled loader looks like — the one reading this screen must not give. It was
/// giving it: `have/want` is `0/280` for the first second of every launch from
/// orbit, and `Some(0.0)` is a number, so the sweep never ran.
const BAR_SWEEP_BELOW: f32 = 0.005;
/// Where the bar stands once there is an anchor and a streamer counting against
/// it — a floor, so "the map has been placed" is visible as a fill rather than
/// as the same empty track as "nothing has happened yet".
const BAR_ANCHORED: f32 = 0.06;
/// The most of the track the streaming phase may claim. The rest belongs to the
/// closing fade; see [`bar_streaming`].
const BAR_STREAMING: f32 = 0.85;
/// Seconds at which the creep has spent half of what the streaming phase may
/// claim — the time constant of `t / (t + tau)`.
///
/// **The bar has to move while nothing is arriving, and that is not a lie.**
/// Measured here 2026-08-27 on the phone tier: the tile fraction was `8/199`
/// when dismissal was decided, so the track sat at 6 % for the whole hold and
/// then ran to full inside the fade. On a handset, where that hold is seconds
/// rather than one, the same shape is the screenshot behind this change — a bar
/// parked near the left margin under a caption that keeps counting, which reads
/// as a loader that has given up.
///
/// Hyperbolic rather than exponential, and asymptotic rather than clamped: it
/// never stops moving and it never arrives on its own, so however long the wait
/// turns out to be there is no second place for the bar to look stuck. What
/// finishes it is the same thing that always finished it — the closing fade.
const BAR_CREEP_TAU: f32 = 2.0;

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
    /// The streaming clause of the status line, or `None` once there is nothing
    /// left to report. Owned rather than `&'static str` because the counts are
    /// the diagnostic half — "Streaming the map… 6/224" is what says whether a
    /// long hold is a slow network or a streamer that never started.
    stage: Option<String>,
    /// How much of the streamer's *current* working set has arrived, or `None`
    /// before there is a set to count. **Not the bar** — see
    /// [`bar_streaming`] for why the two are different numbers.
    streamed: Option<f32>,
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
        Loading {
            stage: Some("Placing the map…".to_owned()),
            streamed: None,
            ready: false,
        }
    } else {
        // In space the flat clipmap is frozen *and* hidden; the globe's quadtree
        // is the layer that will be uncovered, so it is the one to wait for and
        // to report. Asking `MapStream` here is what produced the
        // eighteen-second hold.
        let (what, have, want, ready) = if in_space {
            let (have, want) = globe.tile_progress();
            ("globe", have, want, have > 0)
        } else {
            (
                "map",
                tiles_ready,
                stream.wanted_tiles(),
                // Unchanged from the signal this system has always dismissed on:
                // a spawned ring is a paper-coloured map, which is a world.
                // Holding out for textures instead would make every warm start
                // *longer*, and the complaint about this screen has never been
                // that it is too short.
                stream.has_tiles(),
            )
        };
        Loading {
            stage: Some(if want > 0 {
                format!("Streaming the {what}… {have}/{want}")
            } else {
                format!("Streaming the {what}…")
            }),
            streamed: (want > 0).then(|| (have as f32 / want as f32).clamp(0.0, 1.0)),
            ready,
        }
    }
}

/// The status line: the sign-in clause and the streaming clause, whichever of
/// the two there is.
///
/// **The sign-in rides beside the streaming report, not instead of it**: the two
/// run concurrently, and replacing the tile counts would hide the diagnostic
/// half of the line for exactly the seconds a slow host makes it interesting.
/// The streaming clause is what goes away once dismissal is decided — see
/// [`boot_splash`] — because from then on it is a count nobody is waiting for.
fn caption(signin: Option<&str>, stage: Option<&str>) -> String {
    match (signin, stage) {
        (Some(signin), Some(stage)) => format!("{signin} · {stage}"),
        (Some(only), None) | (None, Some(only)) => only.to_owned(),
        (None, None) => "Ready".to_owned(),
    }
}

/// Where the bar stands while the world is still arriving.
///
/// **The bar and the tile count are deliberately different numbers, and
/// conflating them is what made this meter unbelievable.** The splash lifts on
/// the *first* tile — see [`Loading::ready`] — while the streamer's working set
/// is several hundred tiles that go on arriving long after the splash is gone,
/// and (measured 2026-08-26, `tiles.cartopolis.org` 404ing every z3 tile in row
/// 7) some of which never arrive at all. So a track driven straight off
/// `have/want` was guaranteed to be somewhere around a tenth full at the one
/// moment anyone reads it, and to sit there while the app underneath was
/// already running: a loading bar that has never once finished loading.
///
/// So the streaming phase owns [`BAR_ANCHORED`]..[`BAR_STREAMING`] and is driven
/// by **whichever of the splash's two clocks is further along**: the tile
/// fraction, which is real progress and does move, and the [`BAR_CREEP_TAU`]
/// creep, which moves whether or not anything has arrived. Taking the faster of
/// the two rather than the count alone is the difference between a track that
/// shows a wait passing and one that shows a tenth of a tile set — and on a
/// handset, where the wait is seconds, that difference is the whole complaint.
fn bar_streaming(since_anchor: f32, streamed: Option<f32>) -> f32 {
    let creep = since_anchor.max(0.0) / (since_anchor.max(0.0) + BAR_CREEP_TAU);
    let measured = streamed.unwrap_or(0.0).clamp(0.0, 1.0);
    BAR_ANCHORED + creep.max(measured) * (BAR_STREAMING - BAR_ANCHORED)
}

/// …and where it stands once dismissal is decided: from wherever the streaming
/// phase left it to full, over the fade, so the bar finishes in front of the
/// person who watched it.
///
/// **From `held`, not from zero.** The closing ramp used to be `ease(t)` against
/// a monotone latch, so a bar already at 0.5 had to wait out the third of the
/// fade the ramp spent climbing back to where it already was — a pause at
/// exactly the moment the screen is supposed to be leaving.
fn bar_closing(held: f32, t: f32) -> f32 {
    held + (1.0 - held) * ease(t)
}

/// The sign-in half of the status line, when there is one to report.
///
/// A launch with a remembered session enters it during the splash
/// (`lib.rs` writes the request, `session::drive_session` runs it), and until
/// 2026-08-21 the splash said nothing about it — sign in with "stay signed in"
/// ticked, relaunch, and nothing on screen acknowledged the signing-in that was
/// happening, or that it had worked. So: the busy phases read as themselves
/// ([`SessionStatus::label`] — the sign-in card uses the same words), a session
/// that came up reads as "Signed in as …" for the rest of the hold, and a
/// failure says only that it failed — the *reason* is in the app menu beside
/// "Sign in…", where startup already reports it, and a caption that vanishes
/// with the splash is no place for the one line that explains what to do next.
///
/// Ordinary offline launches return `None` and the splash reads as it always
/// has.
fn signin_caption(session: &SessionState, username: Option<&str>) -> Option<String> {
    if session.error.is_some() {
        return Some("Sign-in failed".to_owned());
    }
    match (&session.status, &session.mode) {
        (status, _) if status.is_busy() => Some(status.label().to_owned()),
        (SessionStatus::Online, SessionMode::Remote { .. }) => Some(match username {
            Some(name) => format!("Signed in as {name}"),
            None => "Signed in".to_owned(),
        }),
        _ => None,
    }
}

#[derive(Resource, Default)]
pub struct BootSplash {
    elapsed: f32,
    /// Counts down through [`FADE_S`] once dismissal begins; `None` until then.
    fading: Option<f32>,
    /// The track fill actually drawn, latched so it can only ever grow.
    ///
    /// **The latch is not cosmetic.** The thing being counted changes under the
    /// meter twice in an ordinary launch: the flat streamer answers first
    /// ("0/326"), then the camera restore puts the view in orbit and the globe's
    /// quadtree answers instead ("0/280"), and each streamer re-decides its own
    /// working set every quarter second as the view refines. Drawn straight,
    /// that is a bar that jumps backwards while the world is loading forwards.
    bar: f32,
    /// [`Self::elapsed`] at the frame a streamer first had a working set to count
    /// against — the zero the creep measures from, so a launch that spends a
    /// second placing the map does not arrive at the streaming phase with the
    /// creep already part-spent.
    anchored_at: Option<f32>,
    /// The fill the streaming phase left behind, captured the frame dismissal is
    /// decided. [`bar_closing`] runs from here rather than from zero.
    held: f32,
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
    session: Res<SessionState>,
    identity: Res<AuthIdentity>,
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
    let signin = signin_caption(&session, identity.username.as_deref());
    let progress = loading(&stream, &globe, in_space, tiles_ready.iter().count());

    // Dismiss on the world being visible, not on a timer alone: fading out into
    // an empty blue void is worse than holding the splash half a second longer.
    // `MAX_VISIBLE_S` is the backstop for when tiles never arrive at all.
    let ready = progress.ready && splash.elapsed >= min_visible_s();
    if (ready || splash.elapsed >= MAX_VISIBLE_S) && splash.fading.is_none() {
        splash.fading = Some(FADE_S);
        // Where the streaming phase got to, before the fade starts moving it.
        splash.held = splash.bar;
    }
    if splash.anchored_at.is_none() && progress.streamed.is_some() {
        splash.anchored_at = Some(splash.elapsed);
    }

    // The dismissal fade, which takes the backdrop with it — this is the layer
    // lifting off the world.
    //
    // Advanced by a dt clamped at *both* ends, because the fade has to survive
    // two opposite frame rates: `MIN_FADE_FRAMES` stops a fast frame from
    // spending the budget in one paint, and `MAX_FADE_STRETCH` stops a slow one
    // from holding a half-transparent splash over a running app for seconds. On
    // a healthy frame rate neither clamp engages and the duration is exactly
    // `FADE_S`.
    let backdrop = match splash.fading {
        None => 1.0,
        Some(left) => {
            let left = left - dt.min(FADE_S / MIN_FADE_FRAMES).max(dt / MAX_FADE_STRETCH);
            splash.fading = Some(left);
            if left <= 0.0 {
                splash.finished = true;
                return;
            }
            ease(left / FADE_S)
        }
    };
    // How far into the dismissal we are, once it has been decided — the closing
    // half of the bar, and the point at which the streaming counts stop being
    // what anyone is waiting for.
    let closing = splash
        .fading
        .map(|left| 1.0 - (left / FADE_S).clamp(0.0, 1.0));
    let target = match (closing, splash.anchored_at) {
        (Some(t), _) => bar_closing(splash.held, t),
        (None, Some(t0)) => bar_streaming(splash.elapsed - t0, progress.streamed),
        // Nothing to count yet — the sweep says so (`BAR_SWEEP_BELOW`).
        (None, None) => 0.0,
    };
    splash.bar = splash.bar.max(target);
    let label = caption(
        signin.as_deref(),
        closing
            .is_none()
            .then_some(progress.stage.as_deref())
            .flatten(),
    );
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
    let track =
        egui::Rect::from_center_size(egui::pos2(c.x, c.y + 104.0), egui::vec2(track_w, TRACK_H));
    painter.rect_filled(
        track,
        TRACK_H * 0.5,
        fade(theme::SURFACE_RAISED, content, 1.0),
    );
    let bar = if splash.bar > BAR_SWEEP_BELOW {
        // A measured fill, from the left.
        egui::Rect::from_min_size(track.min, egui::vec2(track_w * splash.bar, TRACK_H))
    } else {
        // Nothing measured yet, so no number: a segment sweeping the track
        // instead. The alternative is a bar sitting at zero, which is what a
        // *stalled* loader looks like — the one reading this screen must not
        // give.
        let t = (splash.elapsed / SWEEP_S).fract();
        let x = track.min.x + (t * (1.0 + SWEEP_FRAC) - SWEEP_FRAC) * track_w;
        egui::Rect::from_min_size(
            egui::pos2(x, track.min.y),
            egui::vec2(track_w * SWEEP_FRAC, TRACK_H),
        )
        .intersect(track)
    };
    if bar.width() > 0.0 {
        painter.rect_filled(bar, TRACK_H * 0.5, fade(theme::ACCENT, content, 1.0));
    }
    painter.text(
        egui::pos2(c.x, c.y + 128.0),
        egui::Align2::CENTER_CENTER,
        &label,
        egui::FontId::proportional(STATUS_TEXT),
        fade(theme::TEXT_DIM, content, 0.75),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(status: SessionStatus, mode: SessionMode) -> SessionState {
        let mut s = SessionState::default();
        s.status = status;
        s.mode = mode;
        s
    }

    /// An ordinary offline launch keeps the splash it always had: no caption,
    /// nothing about signing in over the bare map.
    #[test]
    fn an_offline_launch_says_nothing_about_signing_in() {
        let s = session(SessionStatus::Offline, SessionMode::Offline);
        assert_eq!(signin_caption(&s, None), None);
    }

    /// A remembered session entered at startup reports its phases in the same
    /// words the sign-in card uses, then confirms who it signed in as.
    #[test]
    fn a_remembered_session_reports_its_progress_and_then_who_you_are() {
        let mode = SessionMode::Remote {
            server: "sim.example".into(),
        };
        for busy in [SessionStatus::Connecting, SessionStatus::Authenticating] {
            let s = session(busy.clone(), mode.clone());
            assert_eq!(signin_caption(&s, None), Some(busy.label().to_owned()));
        }
        let s = session(SessionStatus::Online, mode);
        assert_eq!(
            signin_caption(&s, Some("alice")),
            Some("Signed in as alice".to_owned())
        );
    }

    /// A failed attempt says so and no more — the reason belongs to the app
    /// menu, which outlives the splash.
    #[test]
    fn a_failed_signin_is_named_without_the_reason() {
        // Failure tears the mode down to offline with the reason left behind,
        // so the error must outrank the mode match.
        let mut s = session(SessionStatus::Offline, SessionMode::Offline);
        s.error = Some("sim.example: that password is not right".into());
        assert_eq!(signin_caption(&s, None), Some("Sign-in failed".to_owned()));
    }

    /// The caption rides beside the streaming report rather than replacing it —
    /// the tile counts are the diagnostic half of the line.
    #[test]
    fn the_caption_joins_the_streaming_label() {
        let progress = loading(&MapStream::default(), &GlobeState::default(), false, 0);
        assert_eq!(progress.stage.as_deref(), Some("Placing the map…"));
        assert_eq!(
            caption(Some("Signing in…"), progress.stage.as_deref()),
            "Signing in… · Placing the map…"
        );
        assert!(!progress.ready);
    }

    /// Either clause stands on its own, and with neither there is still a line —
    /// an empty caption under a full track reads as the screen having given up.
    #[test]
    fn each_half_of_the_caption_stands_alone() {
        assert_eq!(
            caption(None, Some("Streaming the map… 3/9")),
            "Streaming the map… 3/9"
        );
        assert_eq!(
            caption(Some("Signed in as alice"), None),
            "Signed in as alice"
        );
        assert_eq!(caption(None, None), "Ready");
    }

    /// **The bar reaches full.** It is the whole point of the rewrite: driven
    /// straight off `have/want` the track was at a tenth when the splash lifted
    /// and stayed there, which is a loading bar that never finishes loading.
    #[test]
    fn the_bar_finishes_over_the_closing_fade() {
        let held = bar_streaming(1.4, Some(0.02));
        assert!(held < BAR_STREAMING);
        // The fade carries whatever is left, and starts moving on its first
        // frame rather than climbing back to where the bar already was.
        assert!(bar_closing(held, 0.1) > held);
        assert!((bar_closing(held, 1.0) - 1.0).abs() < 1e-6);
    }

    /// **The track moves while nothing is arriving.** A handset spends seconds
    /// in the streaming phase with a tile count in the low tens, and the count
    /// alone parked the bar at the anchored floor for all of it.
    #[test]
    fn the_bar_moves_on_time_when_the_count_does_not() {
        let stalled = Some(0.02);
        let (early, later) = (bar_streaming(0.5, stalled), bar_streaming(4.0, stalled));
        assert!(later > early + 0.1, "{early} -> {later}");
        // …and never on its own reaches the ceiling the fade takes over at, so
        // there is no second place for it to look stuck.
        assert!(bar_streaming(60.0, stalled) < BAR_STREAMING);
    }

    /// Real progress still wins when there is more of it than time has spent —
    /// the creep is a floor under the count, not a replacement for it.
    #[test]
    fn a_fast_stream_outruns_the_creep() {
        assert!(bar_streaming(0.2, Some(0.9)) > bar_streaming(0.2, Some(0.1)));
        assert!((bar_streaming(0.0, Some(1.0)) - BAR_STREAMING).abs() < 1e-6);
    }

    /// A streamer that has a set but nothing in it must not paint an empty
    /// track: `0/280` is the first second of every launch from orbit, and an
    /// empty track is what a stalled loader looks like.
    #[test]
    fn a_streamer_with_nothing_yet_still_shows_a_fill() {
        assert!(bar_streaming(0.0, Some(0.0)) > BAR_SWEEP_BELOW);
    }

    /// The counted set changes under the meter — flat streamer to globe
    /// quadtree at the camera restore, then re-decided every quarter second —
    /// so the drawn fill is latched and can only grow.
    #[test]
    fn the_latch_absorbs_a_denominator_that_moves() {
        // 12/326 on the flat streamer, then the view goes to orbit and the
        // globe answers 0/280 instead.
        let mut bar: f32 = 0.0;
        bar = bar.max(bar_streaming(0.4, Some(12.0 / 326.0)));
        let peak = bar;
        bar = bar.max(bar_streaming(0.4, Some(0.0)));
        assert_eq!(bar, peak);
    }

    /// The fade is bounded at both ends: never fewer than `MIN_FADE_FRAMES`
    /// paints, and never more than `MAX_FADE_STRETCH` times its wall-clock
    /// budget. Twelve paints alone took 4.4 s of real time at the 250–800 ms
    /// frames the startup stream produces.
    #[test]
    fn the_fade_is_bounded_in_frames_and_in_seconds() {
        let run = |dt: f32| {
            let (mut left, mut frames) = (FADE_S, 0u32);
            while left > 0.0 {
                left -= dt.min(FADE_S / MIN_FADE_FRAMES).max(dt / MAX_FADE_STRETCH);
                frames += 1;
            }
            (frames, frames as f32 * dt)
        };
        // 60 fps: neither clamp engages, so it is simply `FADE_S`.
        let (frames, wall) = run(1.0 / 60.0);
        assert!(frames >= MIN_FADE_FRAMES as u32);
        assert!((wall - FADE_S).abs() < 0.05, "{wall}");
        // 4 fps, which is what the startup stream measures: still several
        // paints, and no longer four and a half seconds.
        let (frames, wall) = run(0.25);
        assert!(frames >= 4, "{frames}");
        assert!(wall <= FADE_S * MAX_FADE_STRETCH + 0.25, "{wall}");
    }
}
