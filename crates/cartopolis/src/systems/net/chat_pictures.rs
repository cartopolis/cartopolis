//! **Pictures in private chats** (`docs/notes/pictures-in-private-chats.md`).
//!
//! A picture is shrunk to the place-photo size (`utils::photo::prepare`, a
//! 1600 px JPEG), encrypted under a key made for it alone
//! (`cartopolis_e2ee::seal_attachment`), uploaded to the simulator's picture
//! store, and *then* announced in the chat as a `Payload::Picture` carrying the
//! hash and the key. The server holds ciphertext it has no key for, and the
//! sealed message — which only the chat's members can open — is the only place
//! the key exists.
//!
//! Everything slow happens on a worker: re-encoding, encrypting and the upload
//! on the way out; the download, the decryption and the decode on the way in.
//! What the main thread does is turn decoded pixels into an egui texture, once
//! per picture, and the textures are kept here rather than made per frame —
//! a texture created and dropped each frame is the one shape egui's deferred
//! free cannot save (`docs/notes/egui-texture-lifetime.md`).

use bevy::prelude::*;
use bevy::tasks::{IoTaskPool, Task};
use std::collections::{HashMap, VecDeque};

use crate::platform::http::poll_task;

/// Where a picture lives and what opens it: the half of `Payload::Picture` a
/// line keeps.
#[derive(Clone, Debug, PartialEq)]
pub struct PictureRef {
    pub hash: String,
    pub key: [u8; 32],
    pub nonce: [u8; 12],
    pub width: u32,
    pub height: u32,
}

/// A picture somebody asked to send: encoded bytes in any format `image`
/// reads, and the chat it goes to.
pub struct OutgoingPicture {
    pub channel: String,
    pub bytes: Vec<u8>,
    pub caption: String,
    /// `Some([w, h])` when `bytes` are raw RGBA8 pixels — a snapshot of the
    /// screen — rather than an encoded file.
    pub rgba: Option<[u32; 2]>,
}

/// A picture that has been stored and can now be announced.
pub struct Uploaded {
    pub caption: String,
    pub picture: PictureRef,
    /// The JPEG as sent, so the sender's own bubble needs no download.
    pub jpeg: Vec<u8>,
}

/// Most pictures kept decoded at once. A conversation scrolled back through
/// fetches the rest again, from the cache.
const MAX_HELD: usize = 48;

struct Decoded {
    jpeg: Vec<u8>,
    rgba: Vec<u8>,
    size: [usize; 2],
}

enum Held {
    Loading(Task<Result<Decoded, String>>),
    Ready {
        jpeg: Vec<u8>,
        /// Pixels until the texture is made, then gone.
        rgba: Option<(Vec<u8>, [usize; 2])>,
        texture: Option<egui::TextureHandle>,
    },
    Failed(String),
}

/// What the UI and the agent ask about one picture.
pub enum Shown {
    Loading,
    Ready(egui::TextureHandle),
    Failed(String),
}

#[derive(Resource, Default)]
pub struct ChatPictures {
    uploads: Vec<(String, String, Task<Result<Uploaded, String>>)>,
    held: HashMap<String, Held>,
    /// Hashes in the order they were first asked for, for eviction.
    order: VecDeque<String>,
}

impl ChatPictures {
    /// Shrink, encrypt and upload a picture. [`Self::poll_uploads`] hands it
    /// back once the store has it.
    pub fn send(&mut self, out: OutgoingPicture) {
        let OutgoingPicture {
            channel,
            bytes,
            caption,
            rgba,
        } = out;
        let cap = caption.clone();
        let task = IoTaskPool::get().spawn(async move {
            // A snapshot arrives as pixels: give it the shape a picked file has,
            // so both take one path through the shrink and the encode.
            let bytes = match rgba {
                Some([w, h]) => {
                    let image = image::RgbaImage::from_raw(w, h, bytes)
                        .ok_or("the snapshot was the wrong size")?;
                    let mut png = Vec::new();
                    image::DynamicImage::ImageRgba8(image)
                        .to_rgb8()
                        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
                        .map_err(|e| e.to_string())?;
                    png
                }
                None => bytes,
            };
            let prepared = crate::utils::photo::prepare(&bytes)?;
            let jpeg = prepared.full;
            let (w, h) = image::ImageReader::new(std::io::Cursor::new(&jpeg))
                .with_guessed_format()
                .map_err(|e| e.to_string())?
                .into_dimensions()
                .map_err(|e| e.to_string())?;
            let sealed = cartopolis_e2ee::seal_attachment(&jpeg)
                .map_err(|e| format!("could not seal it: {e}"))?;
            let hash = crate::systems::model_assets::api_upload_picture(sealed.bytes).await?;
            Ok(Uploaded {
                caption: cap,
                picture: PictureRef {
                    hash,
                    key: sealed.key,
                    nonce: sealed.nonce,
                    width: w,
                    height: h,
                },
                jpeg,
            })
        });
        self.uploads.push((channel, caption, task));
    }

    /// Uploads that have finished, one way or the other: `(channel, outcome)`.
    pub fn poll_uploads(&mut self) -> Vec<(String, Result<Uploaded, String>)> {
        let mut done = Vec::new();
        self.uploads
            .retain_mut(|(channel, _, task)| match poll_task(task) {
                Some(result) => {
                    done.push((channel.clone(), result));
                    false
                }
                None => true,
            });
        done
    }

    /// Pictures still on their way up, per channel — for "sending…".
    #[must_use]
    pub fn sending(&self, channel: &str) -> usize {
        self.uploads.iter().filter(|(c, ..)| c == channel).count()
    }

    /// Keep a picture this device already has the bytes of — its own, just
    /// sent — so its bubble does not download what it uploaded.
    pub fn insert_local(&mut self, hash: &str, jpeg: Vec<u8>) {
        if self.held.contains_key(hash) {
            return;
        }
        let task = IoTaskPool::get().spawn(async move { decode(jpeg) });
        self.hold(hash, Held::Loading(task));
    }

    /// Start fetching a picture if it is not already here or on its way.
    fn want(&mut self, r: &PictureRef) {
        if self.held.contains_key(&r.hash) {
            return;
        }
        if r.hash == DEMO_HASH {
            let task = IoTaskPool::get().spawn(async move { decode(demo_jpeg()?) });
            self.hold(DEMO_HASH, Held::Loading(task));
            return;
        }
        let r = r.clone();
        let hash = r.hash.clone();
        let task = IoTaskPool::get().spawn(async move {
            let sealed = crate::systems::model_assets::fetch_picture(&r.hash).await?;
            let jpeg = cartopolis_e2ee::open_attachment(&sealed, &r.key, &r.nonce)
                .map_err(|e| e.to_string())?;
            decode(jpeg)
        });
        self.hold(&hash, Held::Loading(task));
    }

    fn hold(&mut self, hash: &str, held: Held) {
        self.held.insert(hash.to_string(), held);
        self.order.push_back(hash.to_string());
        while self.order.len() > MAX_HELD {
            if let Some(old) = self.order.pop_front() {
                // Not one still loading: dropping its task would cancel a fetch
                // somebody is waiting on, and it would only be asked for again.
                if matches!(self.held.get(&old), Some(Held::Loading(_))) {
                    self.order.push_back(old);
                    break;
                }
                self.held.remove(&old);
            }
        }
    }

    /// The picture as a texture, fetching it on first sight.
    pub fn show(&mut self, ctx: &egui::Context, r: &PictureRef) -> Shown {
        self.want(r);
        match self.held.get_mut(&r.hash) {
            Some(Held::Ready { rgba, texture, .. }) => {
                if texture.is_none() {
                    if let Some((pixels, size)) = rgba.take() {
                        let image = egui::ColorImage::from_rgba_unmultiplied(size, &pixels);
                        *texture = Some(ctx.load_texture(
                            format!("chat_picture_{}", &r.hash[..r.hash.len().min(16)]),
                            image,
                            egui::TextureOptions::LINEAR,
                        ));
                    }
                }
                texture
                    .clone()
                    .map_or(Shown::Failed("no pixels".into()), Shown::Ready)
            }
            Some(Held::Failed(why)) => Shown::Failed(why.clone()),
            _ => Shown::Loading,
        }
    }

    /// The picture's JPEG, fetching it on first sight: `None` while it is on
    /// its way. What an agent looks at.
    #[cfg_attr(not(mcp), allow(dead_code))]
    pub fn jpeg(&mut self, r: &PictureRef) -> Option<Result<Vec<u8>, String>> {
        self.want(r);
        match self.held.get(&r.hash) {
            Some(Held::Ready { jpeg, .. }) => Some(Ok(jpeg.clone())),
            Some(Held::Failed(why)) => Some(Err(why.clone())),
            _ => None,
        }
    }

    /// Fold in whatever finished loading.
    fn poll(&mut self) {
        for held in self.held.values_mut() {
            if let Held::Loading(task) = held {
                if let Some(result) = poll_task(task) {
                    *held = match result {
                        Ok(d) => Held::Ready {
                            jpeg: d.jpeg,
                            rgba: Some((d.rgba, d.size)),
                            texture: None,
                        },
                        Err(why) => {
                            tracing::warn!(error = %why, "a chat picture did not load");
                            Held::Failed(why)
                        }
                    };
                }
            }
        }
    }
}

/// The demo chat's picture (`chats::seed_demo_chats`, `--ui chats`): drawn
/// here, never fetched, so a scripted capture with no server shows a picture
/// in the thread. Not a hash anything can produce — it is not hex.
pub const DEMO_HASH: &str = "demo-picture";

/// A [`PictureRef`] for the demo picture.
#[must_use]
pub fn demo_picture() -> PictureRef {
    PictureRef {
        hash: DEMO_HASH.into(),
        key: [0; 32],
        nonce: [0; 12],
        width: 480,
        height: 360,
    }
}

/// A city at dusk in four colours: sky, a low sun, rooftops and a canal.
fn demo_jpeg() -> Result<Vec<u8>, String> {
    let (w, h) = (480u32, 360u32);
    let img = image::RgbImage::from_fn(w, h, |x, y| {
        let fy = y as f32 / h as f32;
        let roof = 0.55 + 0.08 * ((x as f32 / 37.0).floor() % 3.0) / 2.0;
        let (sx, sy) = (x as f32 - 330.0, y as f32 - 150.0);
        if fy > 0.82 {
            image::Rgb([40, 70, 96])
        } else if fy > roof {
            image::Rgb([70, 52, 48])
        } else if sx * sx + sy * sy < 34.0 * 34.0 {
            image::Rgb([255, 214, 150])
        } else {
            let t = fy / roof;
            image::Rgb([
                (250.0 - 120.0 * (1.0 - t)) as u8,
                (150.0 + 40.0 * (1.0 - t)) as u8,
                (110.0 + 120.0 * (1.0 - t)) as u8,
            ])
        }
    });
    let mut out = Vec::new();
    image::DynamicImage::ImageRgb8(img)
        .write_to(
            &mut std::io::Cursor::new(&mut out),
            image::ImageFormat::Jpeg,
        )
        .map_err(|e| e.to_string())?;
    Ok(out)
}

fn decode(jpeg: Vec<u8>) -> Result<Decoded, String> {
    let (rgba, size) = crate::utils::photo::decode_rgba(&jpeg)?;
    Ok(Decoded { jpeg, rgba, size })
}

/// Fold finished fetches into [`ChatPictures`] each frame.
pub fn poll_chat_pictures(mut pictures: ResMut<ChatPictures>) {
    if pictures
        .held
        .values()
        .any(|h| matches!(h, Held::Loading(_)))
    {
        pictures.poll();
    }
}

/// How a picture is drawn at `max_width`: its own aspect, never taller than
/// `max_height`, never wider than it is.
#[must_use]
pub fn fit(r: &PictureRef, max_width: f32, max_height: f32) -> egui::Vec2 {
    let (w, h) = (r.width.max(1) as f32, r.height.max(1) as f32);
    let scale = (max_width / w).min(max_height / h).min(1.0);
    egui::vec2(w * scale, h * scale)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture(w: u32, h: u32) -> PictureRef {
        PictureRef {
            hash: "00".repeat(32),
            key: [0; 32],
            nonce: [0; 12],
            width: w,
            height: h,
        }
    }

    #[test]
    fn a_picture_keeps_its_shape_inside_the_box() {
        let wide = fit(&picture(1600, 900), 240.0, 240.0);
        assert!((wide.x - 240.0).abs() < 0.01 && (wide.y - 135.0).abs() < 0.01);
        let tall = fit(&picture(900, 1600), 240.0, 240.0);
        assert!((tall.y - 240.0).abs() < 0.01 && (tall.x - 135.0).abs() < 0.01);
        // A small picture is not blown up.
        let small = fit(&picture(100, 50), 240.0, 240.0);
        assert_eq!(small, egui::vec2(100.0, 50.0));
    }
}
