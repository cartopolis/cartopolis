//! **`film`: the screen as it plays, for a few seconds.** A still cannot show
//! an avatar walking, a car turning or a loop restarting; a clip can, and a
//! person can watch it. A model cannot, so the answer carries six frames of it
//! tiled into one picture beside the clip.
//!
//! Not a second recorder: the pictures go to the camera's own clip encoder
//! (`camera_screen::recorder`: the phone's VP8, else the pure-Rust one), each a
//! capture of the window — or, headless, of the harness's offscreen target —
//! taken when the encoder is due for one. Unlike `view` nothing is placed or
//! settled: this films whatever the client is doing, which is the point.
//!
//! **No sound.** The camera's clips record the microphone; this does not,
//! because what is said beside the phone is not what is on its screen.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use bevy::prelude::*;
use bevy::render::view::screenshot::{Screenshot, ScreenshotCaptured};
use futures_channel::oneshot;

use super::{McpBridge, Outcome, Reply};
use crate::resources::ScreenshotState;
use crate::systems::camera_screen::recorder::{Grab, Recorder, Source};
use crate::systems::shot_harness::ShotTarget;
use cartopolis_mcp::protocol::{FilmReply, FilmRequest};

/// Frames in the sheet an agent looks at, and how they are laid out.
const SHEET_FRAMES: usize = 6;
const SHEET_COLUMNS: u32 = 3;
/// A sheet tile's longest edge: six of them must fit one picture a context
/// window can afford.
const SHEET_TILE_PX: u32 = 400;
/// How long past its length a film may take to finish encoding before it is
/// given up on.
const FINISH_GRACE_S: f32 = 30.0;

type Slot = Arc<Mutex<Option<Option<Grab>>>>;

/// A film being recorded.
struct Film {
    req: FilmRequest,
    reply: oneshot::Sender<Reply>,
    started: Instant,
    /// Opened on the first picture, whose shape it takes.
    recorder: Option<Recorder>,
    /// A capture asked for and not yet read back, with the clip time it is for.
    in_flight: Option<(Slot, u64)>,
    stills: Vec<Grab>,
    size: [u32; 2],
}

/// The film in progress, if any: one at a time, since they share the screen.
#[derive(Resource, Default)]
pub struct Films {
    queue: Vec<(FilmRequest, oneshot::Sender<Reply>)>,
    active: Option<Film>,
}

impl Films {
    pub fn push(&mut self, req: FilmRequest, reply: oneshot::Sender<Reply>) {
        self.queue.push((req, reply));
    }
}

/// Where a clip goes when the caller did not say.
fn default_out(n: u64) -> PathBuf {
    super::scratch_dir().join(format!("cartopolis-film-{n:04}.webm"))
}

fn copy(g: &Grab) -> Grab {
    Grab {
        pixels: g.pixels.clone(),
        size: g.size,
        bgra: g.bgra,
    }
}

/// Ask for the screen as it is; the pixels arrive in the slot returned
/// (`None` inside when they could not be read).
fn capture(commands: &mut Commands, target: Option<&ShotTarget>) -> Slot {
    let slot: Slot = Arc::default();
    let out = slot.clone();
    let screenshot = match target {
        Some(t) => Screenshot::image(t.0.clone()),
        None => Screenshot::primary_window(),
    };
    commands
        .spawn(screenshot)
        .observe(move |captured: On<ScreenshotCaptured>| {
            *out.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                Some(Grab::from_image(&captured.image));
        });
    slot
}

/// Start the next film, feed the one recording its pictures, and answer it
/// once its clip is written.
pub fn drive_films(
    mut commands: Commands,
    mut films: ResMut<Films>,
    mut shot: ResMut<ScreenshotState>,
    target: Option<Res<ShotTarget>>,
    mut bridge: ResMut<McpBridge>,
    real: Res<Time<Real>>,
    mut made: Local<u64>,
) {
    let now = real.elapsed_secs_f64();
    let films = &mut *films;
    for (req, reply) in std::mem::take(&mut films.queue) {
        if films.active.is_some() {
            let _ = reply.send(Reply::Error("A film is recording already.".into()));
            continue;
        }
        bridge.note(
            "film".into(),
            format!("recording {:.0} s…", req.seconds),
            Outcome::Working,
            now,
        );
        films.active = Some(Film {
            req,
            reply,
            started: Instant::now(),
            recorder: None,
            in_flight: None,
            stills: Vec::new(),
            size: [0, 0],
        });
    }
    let Some(film) = films.active.as_mut() else {
        return;
    };
    let length_ms = (film.req.seconds * 1000.0) as u64;
    let failed = |film: Film, why: String, bridge: &mut McpBridge| {
        bridge.note("film".into(), why.clone(), Outcome::Failed, now);
        let _ = film.reply.send(Reply::Error(why));
    };

    // A picture that has come back goes to the encoder, and maybe the sheet.
    if let Some((slot, ms)) = &film.in_flight {
        let got = slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(grab) = got {
            let ms = *ms;
            film.in_flight = None;
            let Some(grab) = grab else {
                let film = films.active.take().expect("checked above");
                failed(
                    film,
                    "The screen could not be read back.".into(),
                    &mut bridge,
                );
                return;
            };
            if film.recorder.is_none() {
                film.size = grab.size;
                let aspect = grab.size[0] as f32 / grab.size[1].max(1) as f32;
                match Recorder::start(aspect, false, false, Vec::new()) {
                    Ok(r) => film.recorder = Some(r),
                    Err(e) => {
                        let film = films.active.take().expect("checked above");
                        failed(
                            film,
                            format!("The recorder would not start: {e}"),
                            &mut bridge,
                        );
                        return;
                    }
                }
            }
            let due = film.stills.len() as u64 * length_ms / SHEET_FRAMES as u64;
            if film.stills.len() < SHEET_FRAMES && ms >= due {
                film.stills.push(copy(&grab));
            }
            if let Some(r) = film.recorder.as_mut() {
                r.picture(
                    Source::World {
                        grab,
                        rect: [0.0, 0.0, 1.0, 1.0],
                    },
                    ms,
                );
            }
        }
    }

    let elapsed_ms = film.started.elapsed().as_millis() as u64;
    let stopping = film.recorder.as_ref().is_some_and(Recorder::stopping);
    if !stopping {
        let full = film.recorder.as_mut().and_then(Recorder::tick).is_some();
        if full || (elapsed_ms >= length_ms && film.in_flight.is_none()) {
            if let Some(r) = film.recorder.as_mut() {
                r.stop();
            }
        } else if film.in_flight.is_none()
            && elapsed_ms < length_ms
            && film.recorder.as_ref().is_none_or(Recorder::due)
        {
            let ms = film.recorder.as_ref().map_or(0, Recorder::elapsed_ms);
            film.in_flight = Some((capture(&mut commands, target.as_deref()), ms));
            if film.req.hide_ui {
                // This frame's chrome is dropped; `ui::drive_screenshots`
                // brings it back the next.
                shot.hide_ui = true;
            }
        }
    }

    let finished = film.recorder.as_mut().and_then(Recorder::finished);
    let overdue = elapsed_ms as f32 > (film.req.seconds + FINISH_GRACE_S) * 1000.0;
    match finished {
        None if overdue => {
            let film = films.active.take().expect("checked above");
            failed(
                film,
                "The clip did not finish encoding.".into(),
                &mut bridge,
            );
        }
        None => {}
        Some(Err(e)) => {
            let film = films.active.take().expect("checked above");
            failed(film, e, &mut bridge);
        }
        Some(Ok(webm)) => {
            let film = films.active.take().expect("checked above");
            *made += 1;
            let path = film.req.out.clone().unwrap_or_else(|| default_out(*made));
            let (pictures, dropped) = film
                .recorder
                .as_ref()
                .map_or((0, 0), |r| (r.sent, r.dropped));
            bridge.note(
                "film".into(),
                format!("{pictures} pictures, {} kB", webm.len() / 1024),
                Outcome::Done,
                now,
            );
            let reply = film.reply;
            let (size, stills) = (film.size, film.stills);
            let seconds = (elapsed_ms as f32 / 1000.0).min(film.req.seconds);
            // The sheet is six full screens shrunk, and the file a few
            // megabytes: neither on the frame.
            crate::platform::workers::submit(
                "mcp_film",
                Box::new(move || {
                    if let Err(e) = std::fs::write(&path, &webm) {
                        tracing::warn!(path = %path.display(), error = %e, "film: not saved");
                    }
                    let _ = reply.send(Reply::Film(Box::new(FilmReply {
                        webm,
                        path,
                        width: size[0],
                        height: size[1],
                        seconds,
                        pictures,
                        dropped,
                        sheet: sheet(&stills),
                    })));
                }),
            );
        }
    }
}

/// The stills tiled left to right, top to bottom, as a JPEG.
fn sheet(stills: &[Grab]) -> Option<Vec<u8>> {
    let tiles: Vec<image::RgbImage> = stills
        .iter()
        .filter_map(|g| {
            let mut px = g.pixels.clone();
            if g.bgra {
                px.chunks_exact_mut(4).for_each(|p| p.swap(0, 2));
            }
            let img = image::RgbaImage::from_raw(g.size[0], g.size[1], px)?;
            let img = image::DynamicImage::ImageRgba8(img);
            Some(
                img.resize(
                    SHEET_TILE_PX,
                    SHEET_TILE_PX,
                    image::imageops::FilterType::Triangle,
                )
                .to_rgb8(),
            )
        })
        .collect();
    let first = tiles.first()?;
    let (tw, th) = (first.width(), first.height());
    let cols = SHEET_COLUMNS.min(tiles.len() as u32);
    let rows = (tiles.len() as u32).div_ceil(cols);
    let mut out = image::RgbImage::new(tw * cols, th * rows);
    for (i, t) in tiles.iter().enumerate() {
        let (c, r) = (i as u32 % cols, i as u32 / cols);
        image::imageops::overlay(&mut out, t, i64::from(c * tw), i64::from(r * th));
    }
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 80)
        .encode_image(&out)
        .ok()?;
    Some(jpeg)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Six stills make a three-by-two sheet, each tile shrunk, BGRA turned
    /// the right way round.
    #[test]
    fn six_stills_make_a_three_by_two_sheet() {
        let still = |bgra| Grab {
            pixels: [255u8, 0, 0, 255].repeat(800 * 400),
            size: [800, 400],
            bgra,
        };
        let stills: Vec<Grab> = (0..6).map(|i| still(i % 2 == 0)).collect();
        let jpeg = sheet(&stills).expect("a sheet");
        let img = image::load_from_memory(&jpeg).unwrap().to_rgb8();
        assert_eq!((img.width(), img.height()), (3 * 400, 2 * 200));
        // The first tile was BGRA, so its pure "red" bytes are blue.
        let p = img.get_pixel(10, 10);
        assert!(p[2] > 200 && p[0] < 60, "{p:?}");
        let q = img.get_pixel(410, 10);
        assert!(q[0] > 200 && q[2] < 60, "{q:?}");
        assert!(sheet(&[]).is_none());
    }
}
