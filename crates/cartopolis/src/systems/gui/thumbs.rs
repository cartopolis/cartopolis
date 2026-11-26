//! Pictures for the component library's screens as small `data:` URLs: a
//! community's picture in its disc, a post's photo and polaroid, a saved
//! place's thumbnail.
//!
//! **Why a thumbnail, and why here.** An `<img>`'s `data:` source lives in
//! the view and in the HTML a capture writes, and `mithril_bevy` decodes it
//! into an image of its own, so a screen hands over a picture no larger than
//! it draws it: at most twice its drawn size in points (2 px a point covers a
//! phone at scale 1.5 with room, and is twice a desktop's). Scaling and the
//! JPEG encode are CPU work, so they run on `platform::workers`, never on the
//! frame; the answer is kept here by its key and size, so a screen that asks
//! every frame encodes once.
//!
//! **One slot per key and size.** A community's picture in a 32-point row and
//! a 64-point hero are two slots, each fetched and encoded on its own: the
//! fetch is the local cache after the first time (`fetch_picture`), and
//! keeping the source bytes between them would hold a full photo per post.
//!
//! **Bounded.** Past [`MAX_SLOTS`] the least recently asked-for answers are
//! let go; one asked for again is simply made again.
//!
//! On the web the URL is the same: the browser decodes it and lets it go
//! with the element.

use std::collections::HashMap;
use std::sync::Arc;

use bevy::tasks::Task;

use crate::platform::sync::Mutex;

/// How a picture is cut to its thumbnail, in pixels (twice the points it
/// is drawn at).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Fit {
    /// A centre-cropped square of this edge: a disc, a list's tile.
    Square(u32),
    /// The whole picture, scaled down to fit this box, its proportions kept.
    Within(u32, u32),
}

/// A finished thumbnail: its `data:image/jpeg` URL and its size in pixels.
#[derive(Clone, PartialEq, Debug)]
pub struct Thumb {
    /// Shared, so a view that holds it every frame copies a pointer.
    pub url: Arc<str>,
    pub w: u32,
    pub h: u32,
}

impl Thumb {
    /// The size it is drawn at to fill as much of a `max_w` × `max_h` box as
    /// it can with its proportions kept (the library has no `object-fit`).
    #[must_use]
    pub fn fitted(&self, max_w: f32, max_h: f32) -> (f32, f32) {
        let (w, h) = (self.w.max(1) as f32, self.h.max(1) as f32);
        let k = (max_w / w).min(max_h / h).max(0.0);
        ((w * k).round().max(1.0), (h * k).round().max(1.0))
    }
}

/// Where a thumbnail's bytes come from, asked the first time it is wanted.
pub enum Start {
    /// In hand already (a place's stored thumbnail, a scripted capture's).
    Bytes(Vec<u8>),
    /// Still on their way: fetched by this task.
    Fetch(Task<Result<Vec<u8>, String>>),
    /// Not yet; ask again next frame.
    Wait,
}

type Done = Arc<Mutex<Option<Result<Thumb, String>>>>;

enum Stage {
    Fetching(Task<Result<Vec<u8>, String>>),
    Encoding(Done),
    Ready(Thumb),
    Failed,
}

struct Slot {
    stage: Stage,
    /// When it was last asked for, in asks.
    used: u64,
}

/// How many answers are kept before the oldest are let go.
const MAX_SLOTS: usize = 192;

/// JPEG quality of a thumbnail: small pictures show blocking less, and the
/// URL is carried in every view that draws it.
const QUALITY: u8 = 80;

/// Cap on decode allocation, as `utils::photo`'s: a picture from the store
/// is someone's upload, and a panorama must fail rather than take the app.
const MAX_DECODE_BYTES: u64 = 256 * 1024 * 1024;

/// The thumbnails made so far, and those on their way, by key and size.
#[derive(Default)]
pub struct Thumbs {
    slots: HashMap<(String, Fit), Slot>,
    asks: u64,
}

impl Thumbs {
    /// The thumbnail of `key` cut to `fit`: `None` until it is made (or when
    /// it cannot be). The first ask calls `start` for its bytes; every ask
    /// moves it along.
    pub fn get(&mut self, key: &str, fit: Fit, start: impl FnOnce() -> Start) -> Option<Thumb> {
        self.asks += 1;
        let asks = self.asks;
        let slot_key = (key.to_owned(), fit);
        if !self.slots.contains_key(&slot_key) {
            let stage = match start() {
                Start::Bytes(bytes) => encode_on_worker(bytes, fit),
                Start::Fetch(task) => Stage::Fetching(task),
                Start::Wait => return None,
            };
            self.trim();
            self.slots
                .insert(slot_key.clone(), Slot { stage, used: asks });
        }
        let slot = self.slots.get_mut(&slot_key)?;
        slot.used = asks;
        advance(&mut slot.stage, key, fit);
        match &slot.stage {
            Stage::Ready(t) => Some(t.clone()),
            _ => None,
        }
    }

    /// Let the least recently asked-for half go once there are too many.
    fn trim(&mut self) {
        if self.slots.len() < MAX_SLOTS {
            return;
        }
        let mut ages: Vec<u64> = self.slots.values().map(|s| s.used).collect();
        ages.sort_unstable();
        let cut = ages[ages.len() / 2];
        self.slots.retain(|_, s| s.used > cut);
    }
}

/// One step: bytes that arrived go to a worker, an encode that finished is
/// taken.
fn advance(stage: &mut Stage, key: &str, fit: Fit) {
    match stage {
        Stage::Fetching(task) => {
            if let Some(result) = crate::platform::http::poll_task(task) {
                *stage = match result {
                    Ok(bytes) => encode_on_worker(bytes, fit),
                    Err(why) => {
                        tracing::warn!(key, error = %why, "a picture did not load");
                        Stage::Failed
                    }
                };
            }
        }
        Stage::Encoding(done) => {
            let taken = done
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            match taken {
                None => {}
                Some(Ok(t)) => *stage = Stage::Ready(t),
                Some(Err(why)) => {
                    tracing::warn!(key, error = %why, "a picture could not be made small");
                    *stage = Stage::Failed;
                }
            }
        }
        Stage::Ready(_) | Stage::Failed => {}
    }
}

fn encode_on_worker(bytes: Vec<u8>, fit: Fit) -> Stage {
    let done: Done = Arc::default();
    let out = done.clone();
    crate::platform::workers::submit(
        "thumbnail",
        Box::new(move || {
            let made = thumbnail(&bytes, fit);
            *out.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(made);
        }),
    );
    Stage::Encoding(done)
}

/// Decode `bytes` (whatever `image` recognises), turn it upright, cut it to
/// `fit` — never larger than it is — and encode it as a JPEG `data:` URL.
pub fn thumbnail(bytes: &[u8], fit: Fit) -> Result<Thumb, String> {
    use image::ImageDecoder as _;
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| format!("could not read the picture: {e}"))?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(MAX_DECODE_BYTES);
    reader.limits(limits);
    let mut decoder = reader
        .into_decoder()
        .map_err(|e| format!("not a picture this can read: {e}"))?;
    let orientation = decoder
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut img = image::DynamicImage::from_decoder(decoder)
        .map_err(|e| format!("could not decode the picture: {e}"))?;
    img.apply_orientation(orientation);
    let (w, h) = (img.width().max(1), img.height().max(1));
    let filter = image::imageops::FilterType::Triangle;
    let small = match fit {
        Fit::Square(edge) => {
            let edge = edge.min(w).min(h).max(1);
            img.resize_to_fill(edge, edge, filter)
        }
        Fit::Within(mw, mh) => {
            if w <= mw && h <= mh {
                img
            } else {
                // `resize` keeps the proportions inside the box.
                img.resize(mw.max(1), mh.max(1), filter)
            }
        }
    };
    let rgb = small.to_rgb8();
    let (tw, th) = (rgb.width(), rgb.height());
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, QUALITY)
        .encode_image(&rgb)
        .map_err(|e| format!("could not encode the picture: {e}"))?;
    Ok(Thumb {
        url: Arc::from(data_url(&jpeg)),
        w: tw,
        h: th,
    })
}

/// `jpeg` as a `data:image/jpeg;base64,…` URL.
#[must_use]
pub fn data_url(jpeg: &[u8]) -> String {
    format!("data:image/jpeg;base64,{}", base64(jpeg))
}

/// Standard base64 with padding (`lib_splash`'s): the `base64` crate is not
/// in the web build.
fn base64(bytes: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4 + 24);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(ABC[(n >> 18) as usize & 63] as char);
        out.push(ABC[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ABC[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ABC[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jpeg(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbImage::from_fn(w, h, |x, y| image::Rgb([x as u8, y as u8, 90]));
        let mut out = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(
                &mut std::io::Cursor::new(&mut out),
                image::ImageFormat::Jpeg,
            )
            .expect("encodes");
        out
    }

    #[test]
    fn a_square_is_cropped_and_never_grown() {
        let t = thumbnail(&jpeg(300, 200), Fit::Square(64)).expect("made");
        assert_eq!((t.w, t.h), (64, 64));
        assert!(t.url.starts_with("data:image/jpeg;base64,"));
        let small = thumbnail(&jpeg(40, 30), Fit::Square(64)).expect("made");
        assert_eq!((small.w, small.h), (30, 30));
    }

    #[test]
    fn within_keeps_the_proportions() {
        let t = thumbnail(&jpeg(800, 400), Fit::Within(400, 400)).expect("made");
        assert_eq!((t.w, t.h), (400, 200));
        assert_eq!(t.fitted(100.0, 100.0), (100.0, 50.0));
    }

    #[test]
    fn base64_pads_as_the_standard_does() {
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
    }
}
