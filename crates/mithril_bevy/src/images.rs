//! `<img>` for the Bevy backend. Its source is a `data:` URL
//! (`data:image/jpeg;base64,…`), which the browser shows as it is, so one
//! view draws a photo in both backends; here it is decoded into an asset,
//! keyed by the source, and shared by every node showing that source.
//!
//! The cache frees what nothing shows. Each `<img>` node records the source
//! it was last styled with ([`picture`]); once a frame ([`sweep`], at the
//! end of [`crate::drive_ui`]) an entry a mounted node still names is
//! marked used. One no mounted node has named for [`IDLE_FRAMES`] frames
//! leaves the cache, and so does the least recently used of the unshown
//! ones while the decoded pixels exceed [`CAP_BYTES`]; an entry a mounted
//! node names is never evicted, so the cap can be overrun by what is on
//! screen. A source shown again after that decodes again.
//!
//! An evicted image's asset is removed from `Assets<Image>` one sweep
//! later, not in the frame it left: never free a texture in the frame that
//! may have recorded work on it (`docs/notes/texture-lifetime.md`).
//!
//! So a screen may show pictures — avatars, chats, posts — but as
//! thumbnails scaled to what it draws: the URL lives in the view and in
//! the HTML a capture writes. `bevy:` live images are not cached here.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use base64::Engine;
use bevy::asset::RenderAssetUsages;
use bevy::prelude::*;

use crate::tree::UiTree;
use crate::NodeId;

/// Frames an image no mounted node shows stays cached: two seconds at
/// 60 fps, so a screen closed and reopened at once does not decode again.
pub const IDLE_FRAMES: u64 = 120;

/// Decoded pixels the cache keeps before it evicts unshown images early,
/// least recently used first.
pub const CAP_BYTES: usize = 48 * 1024 * 1024;

/// One source: its image (`None` when it did not decode), the bytes its
/// pixels take, and the last sweep a mounted node named it.
struct Entry {
    handle: Option<Handle<Image>>,
    bytes: usize,
    last_used: u64,
}

/// Sources seen, the nodes showing each, and evicted images waiting to be
/// freed.
#[derive(Resource)]
pub(crate) struct ImageCache {
    by_src: HashMap<u64, Entry>,
    /// The source each `<img>` node was last styled with.
    users: HashMap<NodeId, u64>,
    /// Evicted handles and the sweep that evicted them; held (strong)
    /// until a later sweep removes their assets.
    freeing: Vec<(Handle<Image>, u64)>,
    bytes: usize,
    frame: u64,
    cap_bytes: usize,
}

impl Default for ImageCache {
    fn default() -> Self {
        ImageCache {
            by_src: HashMap::new(),
            users: HashMap::new(),
            freeing: Vec::new(),
            bytes: 0,
            frame: 0,
            cap_bytes: CAP_BYTES,
        }
    }
}

/// Images the app draws into, by name, for `<img src="bevy:NAME">`: a
/// camera's viewfinder, a call's video tiles. The handle stays and its
/// pixels change, so a frame shows the frame's picture; nothing is copied
/// into the view, and nothing is cached here. Register the name before
/// the view that shows it mounts — an `<img>` reads its source when it is
/// styled.
#[derive(Resource, Default)]
pub struct LiveImages(pub HashMap<String, Handle<Image>>);

fn key_of(src: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    src.hash(&mut h);
    h.finish()
}

/// The image `<img>` node `user` shows for its source, recording that it
/// shows it (or, for a `bevy:` source or none, that it shows no cached
/// image).
pub(crate) fn picture(world: &mut World, user: NodeId, src: Option<&str>) -> Option<Handle<Image>> {
    let Some(src) = src else {
        world.resource_mut::<ImageCache>().users.remove(&user);
        return None;
    };
    if let Some(name) = src.strip_prefix("bevy:") {
        world.resource_mut::<ImageCache>().users.remove(&user);
        return world
            .get_resource::<LiveImages>()
            .and_then(|live| live.0.get(name).cloned());
    }
    let key = key_of(src);
    let frame = {
        let mut cache = world.resource_mut::<ImageCache>();
        cache.users.insert(user, key);
        cache.frame
    };
    if let Some(seen) = world.resource::<ImageCache>().by_src.get(&key) {
        return seen.handle.clone();
    }
    let decoded = decode(src);
    if decoded.is_none() {
        warn!(
            len = src.len(),
            "mithril_bevy: an <img> source is not a data: URL it can read"
        );
    }
    let bytes = decoded
        .as_ref()
        .and_then(|image| image.data.as_ref())
        .map_or(0, Vec::len);
    let handle = decoded.map(|image| world.resource_mut::<Assets<Image>>().add(image));
    let mut cache = world.resource_mut::<ImageCache>();
    cache.bytes += bytes;
    cache.by_src.insert(
        key,
        Entry {
            handle: handle.clone(),
            bytes,
            last_used: frame,
        },
    );
    handle
}

/// Once a frame, after the tree is styled: frees the assets evicted last
/// sweep, marks what mounted nodes show, and evicts what has gone unshown
/// for [`IDLE_FRAMES`] or overruns the byte cap.
pub(crate) fn sweep(world: &mut World) {
    let gone: Vec<Handle<Image>> = {
        let mut cache = world.resource_mut::<ImageCache>();
        cache.frame += 1;
        let frame = cache.frame;
        let (old, keep) = std::mem::take(&mut cache.freeing)
            .into_iter()
            .partition(|(_, at)| *at < frame);
        cache.freeing = keep;
        old.into_iter().map(|(h, _)| h).collect()
    };
    if !gone.is_empty() {
        let mut assets = world.resource_mut::<Assets<Image>>();
        for h in &gone {
            assets.remove(h.id());
        }
    }
    world.resource_scope(|world, mut cache: Mut<ImageCache>| {
        let tree = world.resource::<UiTree>();
        cache.users.retain(|id, _| tree.nodes.contains_key(id));
    });
    let mut cache = world.resource_mut::<ImageCache>();
    let frame = cache.frame;
    let shown: Vec<u64> = cache.users.values().copied().collect();
    for key in &shown {
        if let Some(e) = cache.by_src.get_mut(key) {
            e.last_used = frame;
        }
    }
    let mut idle: Vec<(u64, u64)> = cache
        .by_src
        .iter()
        .filter(|(_, e)| e.last_used < frame)
        .map(|(k, e)| (e.last_used, *k))
        .collect();
    idle.sort_unstable();
    let mut evict = Vec::new();
    let mut bytes = cache.bytes;
    for (last_used, key) in idle {
        if frame - last_used >= IDLE_FRAMES || bytes > cache.cap_bytes {
            bytes -= cache.by_src[&key].bytes;
            evict.push(key);
        }
    }
    for key in evict {
        if let Some(e) = cache.by_src.remove(&key) {
            cache.bytes -= e.bytes;
            if let Some(h) = e.handle {
                cache.freeing.push((h, frame));
            }
        }
    }
}

fn decode(src: &str) -> Option<Image> {
    let (meta, data) = src.strip_prefix("data:")?.split_once(',')?;
    meta.strip_suffix(";base64")?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .ok()?;
    // The `image` crate, not bevy_image: its JPEG decoder is off in this
    // workspace, and every thumbnail a screen makes is a JPEG.
    let rgba = image::load_from_memory(&bytes).ok()?.to_rgba8();
    let (width, height) = rgba.dimensions();
    Some(Image::new(
        bevy::render::render_resource::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        bevy::render::render_resource::TextureDimension::D2,
        rgba.into_raw(),
        bevy::render::render_resource::TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    ))
}

#[cfg(test)]
mod tests {
    use mithril::view::{el, IntoView};

    use super::*;
    use crate::{drive_ui, with_host};

    /// A 1×1 PNG.
    const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFBQIAX8jx0gAAAABJRU5ErkJggg==";

    /// A 1×1 PNG as a `data:` URL, made distinct by `n` bytes after its
    /// end, which the decoder does not read.
    fn src(n: u8) -> String {
        let engine = &base64::engine::general_purpose::STANDARD;
        let mut bytes = engine.decode(PNG).unwrap();
        bytes.extend(std::iter::repeat_n(0u8, n as usize));
        format!("data:image/png;base64,{}", engine.encode(bytes))
    }

    fn mount(src: &str) {
        let src = src.to_owned();
        with_host(|h| h.mount(el("div").child(el("img").attr("src", src)).into_view()));
    }

    fn cached(world: &World, src: &str) -> bool {
        world
            .resource::<ImageCache>()
            .by_src
            .contains_key(&key_of(src))
    }

    fn asset_of(world: &mut World) -> AssetId<Image> {
        let mut q = world.query::<&ImageNode>();
        let ids: Vec<AssetId<Image>> = q.iter(world).map(|n| n.image.id()).collect();
        assert_eq!(ids.len(), 1, "one <img> drawn");
        ids[0]
    }

    #[test]
    fn reads_a_data_url_and_refuses_the_rest() {
        let image = decode(&format!("data:image/png;base64,{PNG}")).expect("decodes");
        assert_eq!(image.width(), 1);
        assert!(decode(&src(3)).is_some(), "trailing bytes are not read");
        assert!(decode("https://example.org/a.png").is_none());
        assert!(decode("data:image/png,raw").is_none());
    }

    #[test]
    fn reads_a_jpeg() {
        // Every thumbnail a screen makes is a JPEG; bevy_image's decoder for
        // it is off in this workspace, which once left them all blank.
        let mut jpeg = Vec::new();
        image::RgbImage::from_pixel(4, 3, image::Rgb([200, 40, 40]))
            .write_to(
                &mut std::io::Cursor::new(&mut jpeg),
                image::ImageFormat::Jpeg,
            )
            .expect("encodes");
        let b64 = base64::engine::general_purpose::STANDARD.encode(&jpeg);
        let image = decode(&format!("data:image/jpeg;base64,{b64}")).expect("decodes");
        assert_eq!((image.width(), image.height()), (4, 3));
    }

    #[test]
    fn an_image_a_node_shows_stays() {
        let mut world = crate::testing::world();
        let a = src(0);
        mount(&a);
        drive_ui(&mut world);
        let asset = asset_of(&mut world);
        for _ in 0..IDLE_FRAMES * 3 {
            drive_ui(&mut world);
        }
        assert!(cached(&world, &a));
        assert!(world.resource::<Assets<Image>>().contains(asset));
    }

    #[test]
    fn an_unmounted_image_is_freed_and_its_asset_a_frame_later() {
        let mut world = crate::testing::world();
        let a = src(0);
        mount(&a);
        drive_ui(&mut world);
        let asset = asset_of(&mut world);
        with_host(|h| h.clear());
        let mut frames = 0;
        while cached(&world, &a) {
            drive_ui(&mut world);
            frames += 1;
            assert!(frames <= IDLE_FRAMES + 1, "still cached");
            assert!(
                world.resource::<Assets<Image>>().contains(asset),
                "the asset outlives its entry"
            );
        }
        assert_eq!(frames, IDLE_FRAMES);
        assert_eq!(world.resource::<ImageCache>().bytes, 0);
        drive_ui(&mut world);
        assert!(!world.resource::<Assets<Image>>().contains(asset));
        assert!(world.resource::<ImageCache>().freeing.is_empty());
    }

    #[test]
    fn the_byte_cap_evicts_the_least_recently_used_first() {
        let mut world = crate::testing::world();
        // Three 1×1 RGBA images, 4 bytes each; room for two.
        world.resource_mut::<ImageCache>().cap_bytes = 8;
        let (a, b, c) = (src(0), src(1), src(2));
        for s in [&a, &b] {
            mount(s);
            drive_ui(&mut world);
            with_host(|h| h.clear());
            drive_ui(&mut world);
        }
        assert!(cached(&world, &a) && cached(&world, &b));
        // `c` shown: over the cap, and the oldest unshown (`a`) goes.
        mount(&c);
        drive_ui(&mut world);
        assert!(!cached(&world, &a), "least recently used evicted");
        assert!(cached(&world, &b) && cached(&world, &c));
        assert_eq!(world.resource::<ImageCache>().bytes, 8);
    }

    #[test]
    fn a_shown_image_is_never_evicted_for_the_cap() {
        let mut world = crate::testing::world();
        world.resource_mut::<ImageCache>().cap_bytes = 1;
        let a = src(0);
        mount(&a);
        drive_ui(&mut world);
        drive_ui(&mut world);
        assert!(cached(&world, &a));
    }

    #[test]
    fn a_remounted_node_decodes_again() {
        let mut world = crate::testing::world();
        let a = src(0);
        mount(&a);
        drive_ui(&mut world);
        let first = asset_of(&mut world);
        with_host(|h| h.clear());
        for _ in 0..=IDLE_FRAMES + 1 {
            drive_ui(&mut world);
        }
        assert!(!cached(&world, &a));
        mount(&a);
        drive_ui(&mut world);
        assert!(cached(&world, &a));
        let second = asset_of(&mut world);
        assert_ne!(first, second, "decoded afresh");
        assert!(world.resource::<Assets<Image>>().contains(second));
    }

    #[test]
    fn live_images_are_not_cached() {
        let mut world = crate::testing::world();
        let handle = world.resource_mut::<Assets<Image>>().add(Image::default());
        world
            .resource_mut::<LiveImages>()
            .0
            .insert("cam".into(), handle.clone());
        mount("bevy:cam");
        for _ in 0..=IDLE_FRAMES + 1 {
            drive_ui(&mut world);
        }
        assert!(world.resource::<ImageCache>().by_src.is_empty());
        assert!(world.resource::<Assets<Image>>().contains(handle.id()));
        assert_eq!(asset_of(&mut world), handle.id());
    }
}
