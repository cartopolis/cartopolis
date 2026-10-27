//! The avatar as a camera, for a call (`docs/notes/avatar-faces.md`).
//!
//! A call that sends the avatar instead of the person asks for it each frame
//! ([`AvatarVideo::want`]). While it does, a second camera stands in front of
//! the local avatar's face, renders it into a small picture, and that picture
//! is read back and handed to the call as its video — an ordinary picture, so
//! it is sealed and forwarded like a camera's, and the other side needs
//! nothing new. The front camera still runs, for the face only
//! (`face_tracking`): the avatar pulls the person's faces, and only it is
//! seen.

use std::sync::Arc;

use bevy::asset::RenderAssetUsages;
use bevy::camera::{Exposure, RenderTarget};
use bevy::prelude::*;
use bevy::render::gpu_readback::{Readback, ReadbackComplete};
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat, TextureUsages};
use cartopolis_calls::video::I420;

use crate::components::{Avatar, FreeCamera};
use crate::platform::sync::Mutex;
use crate::systems::avatar::AvatarHolder;
use crate::systems::held_phone::HumanArms;

/// The picture's size: 4:3 like a camera's, and a width whose rows are a
/// multiple of 256 bytes, so the read-back rows come without padding.
const SIZE: UVec2 = UVec2::new(512, 384);

/// Pictures a second: a call's camera rate (`video::FPS`).
const FPS: f32 = 15.0;

/// Where the camera stands, in the holder's frame from the head joint at rest
/// (the top of the neck, some 8 cm below the eyes):
/// level with the eyes and three quarters of a metre in front, the framing of a
/// person at a laptop.
const FROM_HEAD: Vec3 = Vec3::new(0.0, 0.08, 0.75);
/// What it looks at, from the head joint: between the eyes and the mouth.
const LOOK_AT: Vec3 = Vec3::new(0.0, 0.05, 0.0);

/// The avatar's own camera.
#[derive(Resource, Default)]
pub(crate) struct AvatarVideo {
    /// Asked for this frame; cleared once read.
    wanted: bool,
    /// The camera, the holder it stands in, and its picture.
    camera: Option<(Entity, Entity, Handle<Image>)>,
    /// The newest picture read back, for the call to take.
    latest: Arc<Mutex<Option<I420>>>,
    /// When a read-back was last asked for.
    asked: Option<web_time::Instant>,
}

impl AvatarVideo {
    /// Film the avatar this frame: call every frame it is wanted.
    pub(crate) fn want(&mut self) {
        self.wanted = true;
    }

    /// The newest picture, once.
    pub(crate) fn take(&self) -> Option<I420> {
        self.latest.lock().ok()?.take()
    }
}

/// Marks the avatar's camera.
#[derive(Component)]
pub(crate) struct AvatarCamera;

/// Put the camera in front of the local avatar's face while it is wanted,
/// take it away when not, and ask for a picture at the call's rate.
#[allow(clippy::too_many_arguments)]
pub(crate) fn drive_avatar_video(
    mut commands: Commands,
    mut video: ResMut<AvatarVideo>,
    mut images: ResMut<Assets<Image>>,
    mut tracking: ResMut<crate::systems::face_tracking::FaceTracking>,
    local: Query<Entity, With<Avatar>>,
    holders: Query<(Entity, &ChildOf, Option<&HumanArms>), With<AvatarHolder>>,
    cameras: Query<(), With<AvatarCamera>>,
    main: Query<&Exposure, With<FreeCamera>>,
) {
    let video = &mut *video;
    if !std::mem::take(&mut video.wanted) {
        if let Some((camera, _, _)) = video.camera.take() {
            commands.entity(camera).try_despawn();
        }
        return;
    }
    tracking.want();
    let Ok(avatar) = local.single() else {
        return;
    };
    // The newest body of the avatar's (a changed look builds a new one
    // beside the old, `human_avatar::finish_rebuilds`).
    let Some((holder, arms)) = holders
        .iter()
        .filter(|(_, child_of, _)| child_of.parent() == avatar)
        .map(|(e, _, arms)| (e, arms))
        .max_by_key(|(e, _)| e.to_bits())
    else {
        return;
    };
    let head = arms.map_or(Vec3::Y * 1.62, |a| a.head);
    let placed = Transform::from_translation(head + FROM_HEAD).looking_at(head + LOOK_AT, Vec3::Y);
    match &video.camera {
        Some((camera, on, _)) if *on == holder && cameras.get(*camera).is_ok() => {
            commands.entity(*camera).try_insert(placed);
        }
        _ => {
            if let Some((old, _, _)) = video.camera.take() {
                commands.entity(old).try_despawn();
            }
            let mut image = Image::new_fill(
                Extent3d {
                    width: SIZE.x,
                    height: SIZE.y,
                    depth_or_array_layers: 1,
                },
                TextureDimension::D2,
                &[0, 0, 0, 255],
                TextureFormat::Rgba8UnormSrgb,
                RenderAssetUsages::RENDER_WORLD,
            );
            image.texture_descriptor.usage = TextureUsages::TEXTURE_BINDING
                | TextureUsages::COPY_SRC
                | TextureUsages::RENDER_ATTACHMENT;
            let handle = images.add(image);
            let camera = commands
                .spawn((
                    Name::new("avatar camera"),
                    AvatarCamera,
                    Camera3d::default(),
                    Camera {
                        // Before the main camera, so its picture is ready
                        // when the frame is read back.
                        order: -1,
                        clear_color: ClearColorConfig::Custom(Color::srgb(0.16, 0.2, 0.21)),
                        ..default()
                    },
                    RenderTarget::Image(handle.clone().into()),
                    // A head and shoulders. The far plane culls nothing: the
                    // city behind is drawn (measured 2026-10-27, buildings
                    // 30 m off in the picture), so this is a second view of
                    // the whole scene, small.
                    Projection::Perspective(PerspectiveProjection {
                        fov: 32f32.to_radians(),
                        near: 0.05,
                        far: 4.0,
                        aspect_ratio: SIZE.x as f32 / SIZE.y as f32,
                        ..default()
                    }),
                    main.single().copied().unwrap_or_default(),
                    placed,
                    ChildOf(holder),
                ))
                .id();
            video.camera = Some((camera, holder, handle));
        }
    }
    // A picture at the call's rate, read back on its own entity, which goes
    // once it has delivered.
    let due = video
        .asked
        .is_none_or(|t| t.elapsed().as_secs_f32() >= 1.0 / FPS);
    if let (true, Some((_, _, image))) = (due, &video.camera) {
        video.asked = Some(web_time::Instant::now());
        let into = Arc::clone(&video.latest);
        commands.spawn(Readback::texture(image.clone())).observe(
            move |done: On<ReadbackComplete>, mut commands: Commands| {
                let rows = SIZE.y as usize;
                let stride = done.data.len() / rows.max(1);
                if let Some(picture) = I420::from_rgba(SIZE.x, SIZE.y, stride, &done.data, false) {
                    if let Ok(mut l) = into.lock() {
                        *l = Some(picture);
                    }
                }
                commands.entity(done.entity).try_despawn();
            },
        );
    }
}

/// `CARTO_AVATAR_VIDEO=/tmp/x.png`: film the avatar as a call would, with no
/// call, and write the pictures to that file as they arrive (each over the
/// last), for a capture to look at what the other side would see.
pub(crate) fn dump_avatar_video(
    mut video: ResMut<AvatarVideo>,
    mut path: Local<Option<Option<String>>>,
) {
    let Some(path) = path
        .get_or_insert_with(|| crate::debug_switches::env_value("CARTO_AVATAR_VIDEO"))
        .as_ref()
    else {
        return;
    };
    video.want();
    let Some(picture) = video.take() else {
        return;
    };
    let rgba = picture.to_rgba();
    if let Some(img) = image::RgbaImage::from_raw(picture.width, picture.height, rgba) {
        if let Err(e) = img.save(path) {
            tracing::warn!("avatar video: {path}: {e}");
        }
    }
}
