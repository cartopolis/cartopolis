//! `<img>` for the Bevy backend. Its source is a `data:` URL
//! (`data:image/jpeg;base64,…`), which the browser shows as it is, so one
//! view draws a photo in both backends; here it is decoded once and kept
//! as an asset for as long as the app runs, keyed by the source.
//!
//! A screen shows thumbnails this way, not full photos: the URL lives in
//! the view and in the HTML a capture writes, so it scales a picture down
//! before it hands it over.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use base64::Engine;
use bevy::asset::RenderAssetUsages;
use bevy::prelude::*;
use bevy_image::{CompressedImageFormats, ImageSampler, ImageType};

/// Sources seen, and the image each decoded to (`None` when it did not).
#[derive(Resource, Default)]
pub(crate) struct ImageCache {
    by_src: HashMap<u64, Option<Handle<Image>>>,
}

/// The image a source names, decoding it the first time it is seen.
pub(crate) fn resolve(world: &mut World, src: &str) -> Option<Handle<Image>> {
    let key = {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        src.hash(&mut h);
        h.finish()
    };
    if let Some(seen) = world.resource::<ImageCache>().by_src.get(&key) {
        return seen.clone();
    }
    let handle = decode(src).map(|image| world.resource_mut::<Assets<Image>>().add(image));
    if handle.is_none() {
        warn!(
            len = src.len(),
            "mithril_bevy: an <img> source is not a data: URL it can read"
        );
    }
    world
        .resource_mut::<ImageCache>()
        .by_src
        .insert(key, handle.clone());
    handle
}

fn decode(src: &str) -> Option<Image> {
    let (meta, data) = src.strip_prefix("data:")?.split_once(',')?;
    let mime = meta.strip_suffix(";base64")?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .ok()?;
    Image::from_buffer(
        &bytes,
        ImageType::MimeType(mime),
        CompressedImageFormats::NONE,
        true,
        ImageSampler::Default,
        RenderAssetUsages::RENDER_WORLD,
    )
    .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_data_url_and_refuses_the_rest() {
        // A 1×1 PNG.
        let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFBQIAX8jx0gAAAABJRU5ErkJggg==";
        let image = decode(&format!("data:image/png;base64,{png}")).expect("decodes");
        assert_eq!(image.width(), 1);
        assert!(decode("https://example.org/a.png").is_none());
        assert!(decode("data:image/png,raw").is_none());
    }
}
