//! A picture open full size, drawn by the component library (`lib_host`):
//! a chat's picture or a saved place's photo (Places ▸ "View the photo").
//!
//! **A chat's picture**: a tap on a picture in the thread opens it, and
//! `chats::drive_chats` hands it over in `ChatsView::lib_viewing` on every
//! frame the Chats panel is open. ✕ is `ChatsView::close_picture`.
//!
//! **A place's photo** is up while `BookmarksState::viewing_photo` names
//! one: set by the Places panel (`Actions::view_photo`), it outlives the
//! panel closing behind it, and closes itself when the photo is removed or
//! replaced or its place deleted. ✕ is `BookmarksState::close_photo`. Its
//! bytes are `photo_store::read_full`, which on the web answers `Loading`
//! until IndexedDB has them. A chat's picture, when one is handed over,
//! goes before a place's photo; the photo shows again once it closes.
//!
//! **The picture is a live image, let go when it closes.** `mithril_bevy`'s
//! `<img>` keeps every `data:` source it decodes for as long as the app runs,
//! so a 1600 px picture per view would be a few MB never given back. Here
//! the JPEG is decoded on a worker (`platform::workers`,
//! `utils::photo::decode_rgba`, EXIF turned upright), made into a Bevy
//! `Image` once the frame's upload allowance admits it
//! (`stream_ledger::admit`), and named — [`NAME`] for a chat's picture,
//! [`PLACE_NAME`] for a place's photo — in `mithril_bevy::LiveImages` before
//! the view that shows it mounts. When the viewer closes or shows another
//! picture the view goes first, then the name, and the asset itself is
//! removed from `Assets<Image>` the frame after — never in the frame the
//! view last drew it. On the web's DOM backend, which has no `bevy:` source,
//! the JPEG goes in as a `data:image/jpeg` URL, which the browser lets go
//! with the element.
//!
//! **Placement.** As egui's window: centred on the screen, the picture as
//! large as fits without growing past its own pixels (egui's `max_size` with
//! `maintain_aspect_ratio`), fitted here in Rust since the library has no
//! `object-fit`. The card may take the screen less its margins or nine
//! tenths of it, whichever is less; egui's opened at eight tenths and could
//! be dragged to nine.
//!
//! **What differs from egui's.** The window cannot be dragged or resized;
//! its title — "Picture", or the place's name — sits in the card with its
//! ✕, as in `lib_dialogs`.

use std::cell::RefCell;
use std::rc::Rc;

use bevy::ecs::system::NonSendMarker;
use bevy::prelude::{warn, Res, ResMut};
#[cfg(all(
    feature = "ui-probe",
    not(all(feature = "ui-dom", target_arch = "wasm32"))
))]
use bevy::prelude::{Assets, Handle, Image};
// The library's, not Bevy's: an explicit import outranks the glob.
use mithril::prelude::Event;
use mithril::prelude::*;

use crate::systems::bookmarks::BookmarksState;
#[cfg(feature = "comms")]
use crate::systems::chat_pictures::{ChatPictures, PictureRef};
#[cfg(feature = "comms")]
use crate::systems::chats::ChatsView;
use crate::systems::lib_settings::LibScreens;
use crate::systems::theme;
use crate::systems::ui_input::UiMetrics;
use crate::systems::user_store::UserStore;

/// The viewer's name in the library's host.
const KEY: &str = "picture_viewer";

/// A chat picture's name in `mithril_bevy::LiveImages`
/// (`<img src="bevy:chat_picture">`).
#[cfg_attr(all(feature = "ui-dom", target_arch = "wasm32"), allow(dead_code))]
pub const NAME: &str = "chat_picture";

/// A place photo's name in `mithril_bevy::LiveImages`
/// (`<img src="bevy:place_photo">`).
#[cfg_attr(all(feature = "ui-dom", target_arch = "wasm32"), allow(dead_code))]
pub const PLACE_NAME: &str = "place_photo";

/// The card's padding, px.
const PAD: f32 = 16.0;

/// The title row's height and the gap under it, px: room enough for the
/// close button.
const HEAD: f32 = 36.0;
const GAP: f32 = 12.0;

/// The card's width while the picture is on its way or failed, before the
/// screen narrows it.
const WAIT_WIDTH: f32 = 320.0;

/// The narrowest card round a small picture, so the title and ✕ fit.
const MIN_WIDTH: f32 = 200.0;

/// What the viewer shows a picture of.
#[derive(Clone)]
enum Source {
    /// A chat's picture, handed over by `chats::drive_chats`.
    #[cfg(feature = "comms")]
    Chat(PictureRef),
    /// A saved place's photo: the place's name and the photo's stem.
    Place { title: String, stem: String },
}

impl Source {
    /// Which picture: another one is let go and fetched afresh.
    fn id(&self) -> String {
        match self {
            #[cfg(feature = "comms")]
            Source::Chat(r) => format!("chat:{}", r.hash),
            Source::Place { stem, .. } => format!("place:{stem}"),
        }
    }

    fn kind(&self) -> Kind {
        match self {
            #[cfg(feature = "comms")]
            Source::Chat(_) => Kind::Chat,
            Source::Place { .. } => Kind::Place,
        }
    }

    /// Its name in `mithril_bevy::LiveImages`, and its decode's label.
    fn name(&self) -> &'static str {
        match self.kind() {
            Kind::Chat => NAME,
            Kind::Place => PLACE_NAME,
        }
    }

    /// The card's title, as egui's window's.
    fn title(&self) -> String {
        match self {
            #[cfg(feature = "comms")]
            Source::Chat(_) => "Picture".to_owned(),
            Source::Place { title, .. } => title.clone(),
        }
    }

    /// Its size as recorded with it, for when the browser's bytes cannot
    /// be measured: a chat's picture carries one, a place's photo none.
    #[cfg(all(feature = "ui-dom", target_arch = "wasm32"))]
    fn recorded_size(&self) -> Option<[usize; 2]> {
        match self {
            #[cfg(feature = "comms")]
            Source::Chat(r) => Some([r.width as usize, r.height as usize]),
            Source::Place { .. } => None,
        }
    }
}

/// Which wording the viewer speaks in.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    #[cfg_attr(not(feature = "comms"), allow(dead_code))]
    Chat,
    Place,
}

/// A picture's bytes: `None` while they are on their way.
type Read = Option<Result<Vec<u8>, String>>;

/// What the viewer shows.
#[derive(Clone, Debug, PartialEq)]
enum Stage {
    Loading,
    /// The picture, its pixels' width and height.
    Ready {
        w: f32,
        h: f32,
    },
    Failed(String),
}

/// What the viewer's taps asked for.
#[derive(Clone, Copy)]
enum Act {
    Close,
}

/// The screen in egui points, and the library's px per point (1 on the Bevy
/// backend, the UI scale as DOM: `lib_place_card::Placement`).
#[derive(Clone, Copy, PartialEq, Debug)]
struct Placement {
    w: f32,
    h: f32,
    k: f32,
}

impl Placement {
    /// The card's largest width and height, points.
    fn card_max(&self) -> (f32, f32) {
        let most = |v: f32| (v * 0.9).min(v - 2.0 * theme::OSD_MARGIN).max(0.0);
        (most(self.w), most(self.h))
    }

    /// A `w` × `h` px picture as large as fits in the card, never larger
    /// than its own pixels, points.
    fn fit(&self, w: f32, h: f32) -> (f32, f32) {
        let (cw, ch) = self.card_max();
        let room_w = (cw - 2.0 * PAD).max(0.0);
        let room_h = (ch - 2.0 * PAD - HEAD - GAP).max(0.0);
        let s = (room_w / w.max(1.0)).min(room_h / h.max(1.0)).min(1.0);
        (w * s, h * s)
    }

    /// The card's width for what it shows, points.
    fn card_width(&self, stage: &Stage) -> f32 {
        let (cw, _) = self.card_max();
        match stage {
            Stage::Ready { w, h } => (self.fit(*w, *h).0 + 2.0 * PAD).max(MIN_WIDTH).min(cw),
            _ => WAIT_WIDTH.min(cw),
        }
    }

    fn px(&self, v: f32) -> String {
        format!("{:.0}px", v * self.k)
    }
}

#[derive(Clone)]
struct Live {
    stage: Stage,
    placement: Placement,
    dark: bool,
    kind: Kind,
    title: String,
}

/// The picture's pixels and where they stand.
#[derive(Default)]
struct Pixels {
    /// Whose: [`Source::id`].
    shown: Option<String>,
    /// What is shown, for the ✕ to close.
    source: Option<Source>,
    /// The name its image goes under in `mithril_bevy::LiveImages`.
    #[cfg_attr(
        not(all(
            feature = "ui-probe",
            not(all(feature = "ui-dom", target_arch = "wasm32"))
        )),
        allow(dead_code)
    )]
    name: &'static str,
    error: Option<String>,
    /// Width and height once ready.
    size: Option<[usize; 2]>,
    /// The decode on its worker.
    #[cfg(all(
        feature = "ui-probe",
        not(all(feature = "ui-dom", target_arch = "wasm32"))
    ))]
    decode: Option<Slot>,
    /// The image named [`Pixels::name`].
    #[cfg(all(
        feature = "ui-probe",
        not(all(feature = "ui-dom", target_arch = "wasm32"))
    ))]
    handle: Option<Handle<Image>>,
    /// Images let go last frame, removed from `Assets<Image>` this one.
    #[cfg(all(
        feature = "ui-probe",
        not(all(feature = "ui-dom", target_arch = "wasm32"))
    ))]
    dropping: Vec<Handle<Image>>,
    /// The JPEG as a `data:` URL.
    #[cfg(all(feature = "ui-dom", target_arch = "wasm32"))]
    src: Option<String>,
}

/// A decode's answer, handed back from its worker.
#[cfg(all(
    feature = "ui-probe",
    not(all(feature = "ui-dom", target_arch = "wasm32"))
))]
type Slot = std::sync::Arc<std::sync::Mutex<Option<Result<(Vec<u8>, [usize; 2]), String>>>>;

thread_local! {
    static MOUNTED: RefCell<Option<Live>> = const { RefCell::new(None) };
    static ACTS: Rc<RefCell<Vec<Act>>> = Rc::new(RefCell::new(Vec::new()));
    static PIXELS: RefCell<Pixels> = RefCell::new(Pixels::default());
}

/// Mounts, bridges and unmounts the viewer, and makes and lets go of its
/// picture. `PostUpdate`, before `DriveUi`, as every library screen — after
/// `chats::drive_chats` (`Update`) has handed a chat's picture over.
#[allow(clippy::too_many_arguments)]
pub fn drive_lib_picture_viewer(
    _main: NonSendMarker,
    lib: Res<LibScreens>,
    #[cfg(feature = "comms")] mut chats: ResMut<ChatsView>,
    #[cfg(feature = "comms")] mut pictures: ResMut<ChatPictures>,
    mut bookmarks: ResMut<BookmarksState>,
    store: Res<UserStore>,
    metrics: Res<UiMetrics>,
    #[cfg(all(
        feature = "ui-probe",
        not(all(feature = "ui-dom", target_arch = "wasm32"))
    ))]
    mut images: ResMut<Assets<Image>>,
    #[cfg(all(
        feature = "ui-probe",
        not(all(feature = "ui-dom", target_arch = "wasm32"))
    ))]
    mut live_images: Option<ResMut<mithril_bevy::LiveImages>>,
) {
    // The taps of the last frame, as egui's window does them: ✕ closes
    // whatever was shown.
    let acts: Vec<Act> = ACTS.with(|a| std::mem::take(&mut *a.borrow_mut()));
    if acts.iter().any(|a| matches!(a, Act::Close)) {
        match PIXELS.with(|px| px.borrow().source.clone()) {
            #[cfg(feature = "comms")]
            Some(Source::Chat(_)) => chats.close_picture(),
            Some(Source::Place { .. }) => bookmarks.close_photo(),
            None => {}
        }
    }

    // A chat's picture, when its viewer is the library's.
    #[cfg(feature = "comms")]
    let chat: Option<Source> = if lib.picture_viewer {
        chats.lib_viewing.take().map(Source::Chat)
    } else {
        None
    };
    #[cfg(not(feature = "comms"))]
    let chat: Option<Source> = {
        let _ = &lib;
        None
    };
    let shown: Option<Source> = chat.or_else(|| {
        bookmarks
            .viewing_photo(&store)
            .map(|(title, stem)| Source::Place { title, stem })
    });

    // Nothing shown and nothing up: nothing to do.
    let idle = shown.is_none()
        && MOUNTED.with(|m| m.borrow().is_none())
        && PIXELS.with(|px| {
            let px = px.borrow();
            #[cfg(all(
                feature = "ui-probe",
                not(all(feature = "ui-dom", target_arch = "wasm32"))
            ))]
            let dropping = !px.dropping.is_empty();
            #[cfg(not(all(
                feature = "ui-probe",
                not(all(feature = "ui-dom", target_arch = "wasm32"))
            )))]
            let dropping = false;
            px.shown.is_none() && !dropping
        });
    if idle {
        return;
    }

    // The picture's bytes, from wherever it lives.
    let mut read = |source: &Source| -> Read {
        match source {
            #[cfg(feature = "comms")]
            Source::Chat(r) => pictures.jpeg(r),
            Source::Place { stem, .. } => place_bytes(stem),
        }
    };

    let k = if cfg!(all(feature = "ui-dom", target_arch = "wasm32")) {
        metrics.ui_scale()
    } else {
        1.0
    };
    let size = metrics.viewport_pts();
    let placement = Placement {
        w: size.x,
        h: size.y,
        k,
    };
    let dark = theme::p().ink == theme::DARK.ink;
    #[cfg(all(
        feature = "ui-probe",
        not(all(feature = "ui-dom", target_arch = "wasm32"))
    ))]
    let mut host = Host {
        images: &mut *images,
        live_images: live_images.as_deref_mut(),
    };
    #[cfg(not(all(
        feature = "ui-probe",
        not(all(feature = "ui-dom", target_arch = "wasm32"))
    )))]
    let mut host = Host(std::marker::PhantomData);

    PIXELS.with(|px| {
        let mut px = px.borrow_mut();
        // What was let go last frame is no longer drawn: its asset goes.
        #[cfg(all(
            feature = "ui-probe",
            not(all(feature = "ui-dom", target_arch = "wasm32"))
        ))]
        for handle in std::mem::take(&mut px.dropping) {
            let _ = host.images.remove(handle.id());
        }

        // Closed, or another picture: the view first, then its pixels.
        let want = shown.as_ref().map(Source::id);
        if px.shown != want {
            MOUNTED.with(|m| {
                if m.borrow_mut().take().is_some() {
                    crate::systems::lib_host::clear_keyed(KEY);
                }
            });
            release(&mut px, &mut host);
            px.shown = want;
            px.name = shown.as_ref().map_or(NAME, Source::name);
        }
        px.source = shown.clone();
        let Some(source) = shown else {
            return;
        };

        // The pixels before the view that shows them: an `<img>` reads its
        // source when it is styled.
        let stage = fetch(&mut px, &source, &mut read, &mut host);
        let kind = source.kind();
        let title = source.title();

        MOUNTED.with(|m| {
            let mut m = m.borrow_mut();
            let stale = m.as_ref().is_some_and(|live| {
                live.stage != stage
                    || live.placement != placement
                    || live.dark != dark
                    || live.title != title
            });
            if stale {
                *m = None;
                crate::systems::lib_host::clear_keyed(KEY);
            }
            if m.is_none() {
                let live = Live {
                    stage,
                    placement,
                    dark,
                    kind,
                    title,
                };
                let src = image_source(&px);
                crate::systems::lib_host::mount_keyed(
                    KEY,
                    view_of(&live, &src),
                    dark,
                    theme::touch_layout(),
                );
                write_html(view_of(&live, &src));
                *m = Some(live);
            }
        });
    });
}

/// A place photo's full-size bytes (`photo_store::read_full`): on the web
/// `None` until IndexedDB answers.
fn place_bytes(stem: &str) -> Read {
    match crate::utils::photo_store::read_full(stem) {
        crate::platform::storage::Cached::Ready(bytes) => Some(Ok(bytes)),
        crate::platform::storage::Cached::Loading => None,
        // Nothing under that key: the photo was there when the row was
        // drawn and is not now.
        crate::platform::storage::Cached::Missing => Some(Err("the photo is gone".to_owned())),
    }
}

/// What the Bevy backend makes the picture in and names it in.
#[cfg(all(
    feature = "ui-probe",
    not(all(feature = "ui-dom", target_arch = "wasm32"))
))]
struct Host<'a> {
    images: &'a mut Assets<Image>,
    live_images: Option<&'a mut mithril_bevy::LiveImages>,
}

/// The other backends need nothing.
#[cfg(not(all(
    feature = "ui-probe",
    not(all(feature = "ui-dom", target_arch = "wasm32"))
)))]
struct Host<'a>(std::marker::PhantomData<&'a ()>);

/// Lets go of the picture: its name now, its asset next frame.
fn release(px: &mut Pixels, host: &mut Host<'_>) {
    #[cfg(all(
        feature = "ui-probe",
        not(all(feature = "ui-dom", target_arch = "wasm32"))
    ))]
    {
        if let Some(handle) = px.handle.take() {
            if let Some(live_images) = host.live_images.as_deref_mut() {
                if live_images.0.get(px.name) == Some(&handle) {
                    live_images.0.remove(px.name);
                }
            }
            px.dropping.push(handle);
        }
        // A decode still running answers into a slot nobody reads.
        px.decode = None;
    }
    #[cfg(all(feature = "ui-dom", target_arch = "wasm32"))]
    {
        px.src = None;
    }
    #[cfg(not(all(
        feature = "ui-probe",
        not(all(feature = "ui-dom", target_arch = "wasm32"))
    )))]
    let _ = host;
    px.error = None;
    px.size = None;
}

/// Where the picture stands, moving it on: asked for, decoding, made into
/// an image and named, or failed.
#[cfg(all(
    feature = "ui-probe",
    not(all(feature = "ui-dom", target_arch = "wasm32"))
))]
fn fetch(
    px: &mut Pixels,
    source: &Source,
    read: &mut dyn FnMut(&Source) -> Read,
    host: &mut Host<'_>,
) -> Stage {
    use bevy::asset::RenderAssetUsages;
    use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

    if let Some(why) = &px.error {
        return Stage::Failed(why.clone());
    }
    if let (Some([w, h]), Some(_)) = (px.size, px.handle.as_ref()) {
        return Stage::Ready {
            w: w as f32,
            h: h as f32,
        };
    }
    let Some(slot) = px.decode.clone() else {
        // Not asked yet: the JPEG, once it is in hand, to a worker.
        match read(source) {
            None => {}
            Some(Err(why)) => px.error = Some(why),
            Some(Ok(jpeg)) => {
                let slot: Slot = std::sync::Arc::default();
                let out = slot.clone();
                crate::platform::workers::submit(
                    source.name(),
                    Box::new(move || {
                        let decoded = crate::utils::photo::decode_rgba(&jpeg);
                        *out.lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(decoded);
                    }),
                );
                px.decode = Some(slot);
            }
        }
        return match &px.error {
            Some(why) => Stage::Failed(why.clone()),
            None => Stage::Loading,
        };
    };
    let done = slot
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    match done {
        None => Stage::Loading,
        Some(Err(why)) => {
            px.decode = None;
            px.error = Some(why.clone());
            Stage::Failed(why)
        }
        Some(Ok((rgba, [w, h]))) => {
            let Some(live_images) = host.live_images.as_deref_mut() else {
                // No library host to name it in: nothing could show it.
                px.decode = None;
                let why = "no picture host".to_owned();
                px.error = Some(why.clone());
                return Stage::Failed(why);
            };
            // Bounded with every other upload: refused, it waits a frame.
            if w == 0 || h == 0 || rgba.len() != w * h * 4 {
                px.decode = None;
                let why = "the picture has no pixels".to_owned();
                px.error = Some(why.clone());
                return Stage::Failed(why);
            }
            if !crate::systems::stream_ledger::admit(rgba.len()) {
                *slot
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Ok((rgba, [w, h])));
                return Stage::Loading;
            }
            let image = Image::new(
                Extent3d {
                    width: w as u32,
                    height: h as u32,
                    depth_or_array_layers: 1,
                },
                TextureDimension::D2,
                rgba,
                TextureFormat::Rgba8UnormSrgb,
                RenderAssetUsages::default(),
            );
            let handle = host.images.add(image);
            live_images.0.insert(px.name.to_owned(), handle.clone());
            px.handle = Some(handle);
            px.size = Some([w, h]);
            px.decode = None;
            Stage::Ready {
                w: w as f32,
                h: h as f32,
            }
        }
    }
}

/// As the Bevy backend's, with the JPEG as a `data:` URL: the browser
/// decodes it, turns it upright and lets it go with the element.
#[cfg(all(feature = "ui-dom", target_arch = "wasm32"))]
fn fetch(
    px: &mut Pixels,
    source: &Source,
    read: &mut dyn FnMut(&Source) -> Read,
    _host: &mut Host<'_>,
) -> Stage {
    if let Some(why) = &px.error {
        return Stage::Failed(why.clone());
    }
    if let (Some([w, h]), Some(_)) = (px.size, px.src.as_ref()) {
        return Stage::Ready {
            w: w as f32,
            h: h as f32,
        };
    }
    match read(source) {
        None => Stage::Loading,
        Some(Err(why)) => {
            px.error = Some(why.clone());
            Stage::Failed(why)
        }
        Some(Ok(jpeg)) => {
            let measured = image::ImageReader::new(std::io::Cursor::new(&jpeg))
                .with_guessed_format()
                .ok()
                .and_then(|r| r.into_dimensions().ok())
                .map(|(w, h)| [w as usize, h as usize]);
            let Some([w, h]) = measured.or_else(|| source.recorded_size()) else {
                let why = "the picture has no pixels".to_owned();
                px.error = Some(why.clone());
                return Stage::Failed(why);
            };
            px.src = Some(format!("data:image/jpeg;base64,{}", base64(&jpeg)));
            px.size = Some([w, h]);
            Stage::Ready {
                w: w as f32,
                h: h as f32,
            }
        }
    }
}

/// With neither backend nothing is drawn: a chat's picture is only asked
/// for, and a place's photo, which would be a file read a frame, is not.
#[cfg(not(any(
    all(
        feature = "ui-probe",
        not(all(feature = "ui-dom", target_arch = "wasm32"))
    ),
    all(feature = "ui-dom", target_arch = "wasm32")
)))]
fn fetch(
    px: &mut Pixels,
    source: &Source,
    read: &mut dyn FnMut(&Source) -> Read,
    _host: &mut Host<'_>,
) -> Stage {
    if source.kind() == Kind::Place {
        return Stage::Loading;
    }
    match read(source) {
        Some(Err(why)) => {
            px.error = Some(why.clone());
            Stage::Failed(why)
        }
        _ => Stage::Loading,
    }
}

/// The `<img>`'s source.
fn image_source(px: &Pixels) -> String {
    #[cfg(all(feature = "ui-dom", target_arch = "wasm32"))]
    {
        px.src.clone().unwrap_or_default()
    }
    #[cfg(not(all(feature = "ui-dom", target_arch = "wasm32")))]
    {
        format!("bevy:{}", px.name)
    }
}

/// Standard base64 with padding (`lib_splash`'s): the `base64` crate is not
/// in the web build.
#[cfg(all(feature = "ui-dom", target_arch = "wasm32"))]
fn base64(bytes: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
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

/// The tap that closes the viewer.
fn close() -> Option<Handler> {
    let acts = ACTS.with(Rc::clone);
    Some(Rc::new(move |_: &Event| acts.borrow_mut().push(Act::Close)) as Handler)
}

/// The viewer: its title and ✕, and under them the picture, a spinner, or
/// why it could not be opened — a chat's picture and a place's photo each
/// in the words egui's window used. The card hangs in a column of no width
/// (`lib_dialogs`), so only the card holds the pointer; its own node
/// listens, so the pointer over its padding is the library's too.
fn view_of(l: &Live, src: &str) -> View {
    let p = l.placement;
    let card_w = p.card_width(&l.stage);
    let outer = format!(
        "position: absolute; left: 0px; top: 0px; width: 0px; height: {}; \
         display: flex; flex-direction: column; justify-content: center; \
         align-items: flex-start",
        p.px(p.h)
    );
    let card = format!(
        "flex: none; margin-left: {}; width: {}; box-sizing: border-box; \
         display: flex; flex-direction: column; gap: {}; padding: {}; \
         background: var(--m-surface); border: 1px solid var(--m-border); \
         border-radius: var(--m-radius-lg); box-shadow: var(--m-shadow-lg)",
        p.px(((p.w - card_w) / 2.0).max(0.0)),
        p.px(card_w),
        p.px(GAP),
        p.px(PAD)
    );
    let title_style = format!(
        "flex: 1; min-width: 0; margin: 0; font-size: {}; font-weight: 600; \
         color: var(--m-text-strong); overflow: hidden; text-overflow: ellipsis; \
         white-space: nowrap",
        p.px(theme::SIZE_TITLE)
    );
    let title = l.title.clone();
    let body: View = match &l.stage {
        Stage::Loading => match l.kind {
            Kind::Chat => view! {
                <div style="display: flex; justify-content: center; padding: 24px 0">
                    <Spinner size=Size::Sm />
                </div>
            },
            // egui's place viewer said what it was waiting for.
            Kind::Place => {
                let style = format!(
                    "display: flex; align-items: center; gap: 8px; padding: 24px 0; \
                     color: var(--m-text-dim); font-size: {}",
                    p.px(13.0)
                );
                view! {
                    <div style={style}>
                        <Spinner size=Size::Sm />
                        <span>"Loading the photo…"</span>
                    </div>
                }
            }
        },
        Stage::Failed(why) => {
            let text = match l.kind {
                Kind::Chat => format!("This picture could not be opened: {why}"),
                Kind::Place => "This photo could not be opened.".to_owned(),
            };
            let style = format!(
                "margin: 0; color: var(--m-warning); font-size: {}",
                p.px(13.0)
            );
            view! { <p style={style}>{text}</p> }
        }
        Stage::Ready { w, h } => {
            let (iw, ih) = p.fit(*w, *h);
            let alt = match l.kind {
                Kind::Chat => "The picture",
                Kind::Place => "The photo",
            };
            let img = el("img")
                .attr("src", src.to_owned())
                .attr("alt", alt)
                .attr(
                    "style",
                    format!(
                        "flex: none; width: {}; height: {}; border-radius: {}; \
                         pointer-events: none",
                        p.px(iw),
                        p.px(ih),
                        p.px(4.0)
                    ),
                )
                .into_view();
            view! { <div style="display: flex; justify-content: center">{img}</div> }
        }
    };
    view! {
        <div style={outer}>
            <div style={card} on:click={|_: &Event| {}}>
                <div style="display: flex; align-items: center; gap: 12px">
                    <h1 style={title_style}>{title}</h1>
                    <CloseButton onclick={close()} />
                </div>
                {body}
            </div>
        </div>
    }
}

/// The HTML from the library's string renderer, for the browser beside a
/// capture.
fn write_html(view: View) {
    let Some(path) = std::env::var_os("CARTO_UI_LIB_HTML") else {
        return;
    };
    use mithril::view::{mount, StringRenderer};
    let r = Rc::new(RefCell::new(StringRenderer::new()));
    let root = r.borrow().root();
    let mounted = mount(view, &r, &root, None);
    let out = r.borrow().html();
    drop(mounted);
    if let Err(e) = std::fs::write(&path, out) {
        warn!(?path, error = %e, "lib picture viewer: could not write the HTML");
    }
}
