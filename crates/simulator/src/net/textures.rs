//! The texture catalogue and texture downloads.

use super::{ConnCtx, FrameTransport};
use crate::textures;
use cartopolis_core::NetMessage;

pub(super) async fn on_catalog<T: FrameTransport>(ctx: &mut ConnCtx<T>) -> anyhow::Result<()> {
    // `spawn_blocking`, because this reads a directory and then a manifest file
    // off the disk. Both are ordinary synchronous syscalls, and running them here
    // would park this worker thread — one of the few that also carry the 20 Hz
    // tick — for as long as the filesystem takes to answer.
    let dir = ctx.config.texture_dir.clone();
    let entries = tokio::task::spawn_blocking(move || textures::list_textures(&dir))
        .await
        .unwrap_or_default();
    ctx.send(&NetMessage::TextureCatalog { textures: entries })
        .await
}

pub(super) async fn on_texture<T: FrameTransport>(
    ctx: &mut ConnCtx<T>,
    request_id: u32,
    texture_id: String,
) -> anyhow::Result<()> {
    // Same reason, and more of it: a texture is a whole file read, sized by
    // whatever the operator put in the directory.
    let loaded = {
        let dir = ctx.config.texture_dir.clone();
        let id = texture_id.clone();
        tokio::task::spawn_blocking(move || textures::load_texture(&dir, &id))
            .await
            .unwrap_or(None)
    };
    match loaded {
        Some(png_bytes) => {
            ctx.send(&NetMessage::TextureData {
                request_id,
                texture_id,
                png_bytes,
            })
            .await
        }
        None => {
            ctx.send(&NetMessage::TextureNotFound {
                request_id,
                texture_id,
            })
            .await
        }
    }
}
