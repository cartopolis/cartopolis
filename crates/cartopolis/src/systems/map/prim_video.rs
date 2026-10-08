//! Video on a prim (protocol 28, `docs/notes/video-on-a-prim.md`): a clip from
//! the simulator's video store, played on the prim's surface for everybody
//! near it, in step with each other.
//!
//! The decoding is `cartopolis_calls::clip`, on a thread per clip — the same
//! VP8 and Opus decoders a call uses. This module decides *which* prims play,
//! fetches their clips, and puts each new picture on the prim's material.
//!
//! **What bounds it is the upload, not the decode.** A picture goes to the GPU
//! whole every time it changes, and the documented way this client loses a
//! phone's GPU is too many bytes in one frame (`docs/notes/upload-bursts.md`).
//! So: only prims within [`RANGE_M`] play; at most [`MAX_PLAYING`] at once,
//! nearest first (one on a phone); pictures are scaled to [`MAX_WIDTH`] before
//! they leave the decoder thread; and every picture is admitted through
//! `stream_ledger`, the per-frame upload budget every streamer shares — a
//! picture the budget refuses is dropped, which a player makes up for with the
//! next one.
//!
//! The screen is drawn unlit: a screen gives light, it does not take the
//! sun's. Stopping a clip (or walking away) hands the prim back to its own
//! texture through `NeedsTextureRefresh`.

use crate::platform::sync::{Arc, Mutex};
use std::collections::HashMap;

use bevy::asset::RenderAssetUsages;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy::tasks::IoTaskPool;
use cartopolis_calls::clip::{ClipPlayer, PlayOptions, SoundRing};
use cartopolis_core::{NetMessage, PrimVideo, SetVideo};

use crate::components::{FreeCamera, NeedsTextureRefresh, OwnedMaterial, Prim};
use crate::platform::model_file;
use crate::resources::{EditDialogState, OnlineSession, VideoAction};
use crate::systems::rendering::owned_material;
use crate::systems::{model_assets, stream_ledger, voice};

/// A prim further than this from the camera does not play.
const RANGE_M: f32 = 60.0;

/// Clips playing at once, nearest first. A phone decodes on the CPU it renders
/// with and uploads through the budget its GPU dies of.
#[cfg(target_os = "android")]
const MAX_PLAYING: usize = 1;
#[cfg(not(target_os = "android"))]
const MAX_PLAYING: usize = 4;

/// Pictures wider than this are scaled down on the decoder thread.
#[cfg(target_os = "android")]
const MAX_WIDTH: u32 = 320;
#[cfg(not(target_os = "android"))]
const MAX_WIDTH: u32 = 640;

type Fetch = Arc<Mutex<Option<Result<Vec<u8>, String>>>>;

enum Clip {
    Fetching(Fetch),
    Ready(Arc<Vec<u8>>),
    Failed,
}

struct Screen {
    video: PrimVideo,
    player: ClipPlayer,
    image: Handle<Image>,
    size: (u32, u32),
    sound: Option<Entity>,
}

/// Every clip this client has and every prim it is playing on.
#[derive(Resource, Default)]
pub struct PrimVideos {
    clips: HashMap<String, Clip>,
    screens: HashMap<Entity, Screen>,
}

impl PrimVideos {
    /// How many prims are playing (the agent's `play_video` answer).
    #[cfg(mcp)]
    #[must_use]
    pub fn playing(&self) -> usize {
        self.screens.len()
    }
}

/// Marks the entity that plays a prim's sound.
#[derive(Component)]
pub struct PrimVideoSound;

/// Choose which prims play, keep their players fed, and draw their pictures.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn drive_prim_videos(
    mut commands: Commands,
    mut videos: ResMut<PrimVideos>,
    prims: Query<(
        Entity,
        &Prim,
        &GlobalTransform,
        Option<&MeshMaterial3d<StandardMaterial>>,
        Has<OwnedMaterial>,
    )>,
    camera: Query<&GlobalTransform, With<FreeCamera>>,
    mut images: ResMut<Assets<Image>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    #[cfg(feature = "audio")] mut streams: ResMut<Assets<voice::VoiceStream>>,
    #[cfg(feature = "audio")] mut sinks: Query<&mut bevy::audio::AudioSink, With<PrimVideoSound>>,
    prefs: Res<crate::resources::AppPrefs>,
) {
    let Ok(eye) = camera.single().map(GlobalTransform::translation) else {
        return;
    };

    // Which prims should be playing: in range, nearest first, up to the cap.
    let mut wanted: Vec<(f32, Entity, PrimVideo)> = prims
        .iter()
        .filter_map(|(e, p, t, material, _)| {
            let v = p.video.clone()?;
            // A glTF model has no single material to paint; only a prim's own
            // shape can be a screen.
            material?;
            let d = t.translation().distance(eye);
            (d <= RANGE_M).then_some((d, e, v))
        })
        .collect();
    wanted.sort_by(|a, b| a.0.total_cmp(&b.0));
    wanted.truncate(MAX_PLAYING);

    // Stop what should not be playing, or is playing something else now.
    let keep: HashMap<Entity, &PrimVideo> = wanted.iter().map(|(_, e, v)| (*e, v)).collect();
    let stale: Vec<Entity> = videos
        .screens
        .iter()
        .filter(|(e, s)| keep.get(e).is_none_or(|v| **v != s.video))
        .map(|(e, _)| *e)
        .collect();
    for e in stale {
        if let Some(s) = videos.screens.remove(&e) {
            if let Some(sound) = s.sound {
                commands.entity(sound).try_despawn();
            }
            images.remove(&s.image);
            // Back to the prim's own texture and surface.
            commands.entity(e).try_insert(NeedsTextureRefresh);
        }
    }

    // Fetch and start what should be.
    for (_, e, v) in &wanted {
        if videos.screens.contains_key(e) {
            continue;
        }
        let clip = videos
            .clips
            .entry(v.asset.clone())
            .or_insert_with(|| start_fetch(v.asset.clone()));
        if let Clip::Fetching(slot) = clip {
            let done = slot.lock().ok().and_then(|mut s| s.take());
            match done {
                Some(Ok(bytes)) => *clip = Clip::Ready(Arc::new(bytes)),
                Some(Err(e)) => {
                    tracing::warn!(asset = %v.asset, "video could not be fetched: {e}");
                    *clip = Clip::Failed;
                }
                None => continue,
            }
        }
        let Clip::Ready(bytes) = clip else { continue };
        let ring: SoundRing = Arc::default();
        let player = match ClipPlayer::start(
            bytes.clone(),
            PlayOptions {
                started_at_ms: v.started_at_ms,
                looped: v.looped,
                max_width: MAX_WIDTH,
                sound: Some((voice::VOICE_RATE, ring.clone())),
            },
        ) {
            Ok(p) => p,
            Err(err) => {
                tracing::warn!(asset = %v.asset, "video will not play: {err}");
                *clip = Clip::Failed;
                continue;
            }
        };
        #[cfg(feature = "audio")]
        let sound = player.info.has_audio.then(|| {
            let handle = streams.add(voice::VoiceStream::from_ring(ring));
            commands
                .spawn((
                    bevy::audio::AudioPlayer(handle),
                    bevy::audio::PlaybackSettings {
                        mode: bevy::audio::PlaybackMode::Once,
                        volume: bevy::audio::Volume::Linear(0.0),
                        ..default()
                    },
                    PrimVideoSound,
                ))
                .id()
        });
        #[cfg(not(feature = "audio"))]
        let sound = {
            let _ = ring;
            None
        };
        let image = images.add(black(2, 2));
        videos.screens.insert(
            *e,
            Screen {
                video: v.clone(),
                player,
                image,
                size: (2, 2),
                sound,
            },
        );
    }

    // New pictures onto their prims, and sound levelled by distance.
    let gain = prefs.voice_gain();
    for (e, screen) in &mut videos.screens {
        let Ok((_, _, t, material, owned)) = prims.get(*e) else {
            continue;
        };
        if let Some(pic) = screen.player.take_picture() {
            // The upload budget first: a picture it refuses is dropped, and the
            // player's next one takes its place.
            if stream_ledger::admit(pic.rgba.len()) {
                if (pic.width, pic.height) != screen.size {
                    images.remove(&screen.image);
                    screen.image = images.add(black(pic.width, pic.height));
                    screen.size = (pic.width, pic.height);
                }
                if let Some(mut img) = images.get_mut(&screen.image) {
                    img.data = Some(pic.rgba);
                }
            }
        }
        // Keep the prim wearing the picture: `refresh_prim_textures` puts the
        // prim's own texture back whenever the prim is edited.
        if let Some(material) = material {
            let already = materials
                .get(&material.0)
                .is_some_and(|m| m.base_color_texture.as_ref() == Some(&screen.image) && m.unlit);
            if !already {
                if let Some(mut m) =
                    owned_material(&mut commands, &mut materials, *e, &material.0, owned)
                {
                    m.base_color_texture = Some(screen.image.clone());
                    m.base_color = Color::WHITE;
                    m.unlit = true;
                }
            }
        }
        #[cfg(feature = "audio")]
        if let Some(sink) = screen.sound.and_then(|s| sinks.get_mut(s).ok()) {
            let mut sink = sink;
            let d = t.translation().distance(eye);
            sink.set_volume(bevy::audio::Volume::Linear(
                voice::attenuation(Some(d)) * gain,
            ));
        }
        #[cfg(not(feature = "audio"))]
        let _ = (t, gain);
    }
}

fn start_fetch(hash: String) -> Clip {
    let slot: Fetch = Arc::default();
    let out = slot.clone();
    IoTaskPool::get()
        .spawn(async move {
            let got = model_assets::fetch_video(&hash).await;
            if let Ok(mut s) = out.lock() {
                *s = Some(got);
            }
        })
        .detach();
    Clip::Fetching(slot)
}

fn black(width: u32, height: u32) -> Image {
    Image::new_fill(
        Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        &[0, 0, 0, 255],
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::default(),
    )
}

/// Where the edit dialog's Video row stands: a file dialog open, or an upload
/// on its way, each for the prim it was started on — the dialog may have moved
/// on to another prim by the time either finishes.
#[derive(Resource, Default)]
pub struct VideoPicker {
    pick: Option<(i64, model_file::Pick)>,
    upload: Option<(i64, Fetch)>,
}

/// Carry out the edit dialog's Video row: choose a clip, upload it, and start
/// it on the prim — or stop what is playing. Starting and stopping are their
/// own message (`SetPrimVideo`), never part of the dialog's save.
pub fn drive_video_actions(
    mut picker: ResMut<VideoPicker>,
    mut dialog: ResMut<EditDialogState>,
    online: Option<Res<OnlineSession>>,
) {
    let Some(online) = online else {
        return;
    };
    match dialog.video_action.take() {
        Some(VideoAction::Choose) if picker.pick.is_none() && picker.upload.is_none() => {
            if let Some(prim_id) = dialog.prim_id {
                match model_file::start_for(model_file::Kind::Video) {
                    Ok(pick) => {
                        picker.pick = Some((prim_id, pick));
                        dialog.video_note = Some("Choosing…".into());
                    }
                    Err(e) => dialog.video_note = Some(e),
                }
            }
        }
        Some(VideoAction::Choose) => {}
        Some(VideoAction::Stop) => {
            if let Some(prim_id) = dialog.prim_id {
                set_video(&online, prim_id, None);
                dialog.playing_video = false;
                dialog.video_note = None;
            }
        }
        None => {}
    }

    let mut note = None;
    if let Some((prim_id, pick)) = picker.pick.as_mut() {
        let prim_id = *prim_id;
        match pick.poll() {
            model_file::Outcome::Pending => {}
            model_file::Outcome::Ready(bytes) => {
                picker.pick = None;
                note = Some(format!("Uploading {} KB…", bytes.len() / 1024));
                let slot: Fetch = Arc::default();
                let out = slot.clone();
                IoTaskPool::get()
                    .spawn(async move {
                        let got = model_assets::api_upload_video(bytes).await;
                        if let Ok(mut s) = out.lock() {
                            *s = Some(got.map(String::into_bytes));
                        }
                    })
                    .detach();
                picker.upload = Some((prim_id, slot));
            }
            model_file::Outcome::Cancelled => {
                picker.pick = None;
                note = Some("No file chosen.".into());
            }
            model_file::Outcome::Failed(e) => {
                picker.pick = None;
                note = Some(e);
            }
        }
    }
    if let Some((prim_id, slot)) = picker.upload.as_ref() {
        let prim_id = *prim_id;
        let done = slot.lock().ok().and_then(|mut s| s.take());
        if let Some(reply) = done {
            picker.upload = None;
            note = Some(match reply.map(|b| uploaded_hash(&b)) {
                Ok(Some(asset)) => {
                    set_video(&online, prim_id, Some(asset));
                    if dialog.prim_id == Some(prim_id) {
                        dialog.playing_video = true;
                    }
                    "Playing.".into()
                }
                Ok(None) => "The server did not say which clip it stored.".into(),
                // The server's refusals are written to be shown.
                Err(e) => refusal(&e),
            });
        }
    }
    if note.is_some() {
        dialog.video_note = note;
    }
}

fn set_video(online: &OnlineSession, prim_id: i64, asset: Option<String>) {
    online.send(
        "prim video",
        NetMessage::SetPrimVideo {
            request_id: 0,
            prim_id,
            video: asset.map(|asset| SetVideo {
                asset,
                looped: true,
            }),
        },
    );
}

/// The `hash` of the upload's answer.
fn uploaded_hash(body: &[u8]) -> Option<String> {
    let json: serde_json::Value = serde_json::from_slice(body).ok()?;
    json["hash"].as_str().map(str::to_string)
}

/// A refusal's `error` line, if the answer carries one.
fn refusal(raw: &str) -> String {
    raw.find(r#"{"error""#)
        .and_then(|start| serde_json::from_str::<serde_json::Value>(&raw[start..]).ok())
        .and_then(|json| json["error"].as_str().map(str::to_string))
        .unwrap_or_else(|| raw.to_string())
}
