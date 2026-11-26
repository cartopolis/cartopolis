//! Where the component library's screens are drawn: through `bevy_ui`
//! (`mithril_bevy`, native, feature `ui-probe`) or as DOM over the canvas
//! (`mithril::dom`, the web build, feature `ui-dom`). A screen's system
//! calls [`mount`] and [`clear`] and never knows which.
//!
//! On the web the client is only the world inside the site's frame
//! (`web-client-is-the-world.md`), so its chrome drawn as DOM takes the
//! site's own look: the library's stylesheets are written into the page
//! from cartopolis' tokens (`theme::tokens`, the same table the site's
//! `theme.css` comes from) the first time something mounts, into a
//! `<style>` of our own, and the screens go into a full-window container
//! that lets pointer events through except where a screen is.

use mithril::view::View;

/// Mounts a screen under `key` (its name), dark or light, for a coarse
/// pointer or not, beside whatever else is mounted (a card over the map
/// beside a panel); [`clear_keyed`] takes it down alone.
pub fn mount_keyed(key: &str, view: View, dark: bool, coarse_pointer: bool) {
    #[cfg(all(feature = "ui-dom", target_arch = "wasm32"))]
    {
        dom::mount_keyed(key, view, dark, coarse_pointer);
    }
    #[cfg(not(all(feature = "ui-dom", target_arch = "wasm32")))]
    {
        #[cfg(feature = "ui-probe")]
        mithril_bevy::with_host(|host| {
            host.env.dark = dark;
            host.env.coarse_pointer = coarse_pointer;
            host.mount_keyed(key, view);
        });
        #[cfg(not(feature = "ui-probe"))]
        let _ = (key, view, dark, coarse_pointer);
    }
}

/// Unmounts the screen under `key`.
pub fn clear_keyed(key: &str) {
    #[cfg(all(feature = "ui-dom", target_arch = "wasm32"))]
    {
        dom::clear_keyed(key);
    }
    #[cfg(not(all(feature = "ui-dom", target_arch = "wasm32")))]
    {
        #[cfg(feature = "ui-probe")]
        mithril_bevy::with_host(|host| host.clear_keyed(key));
        #[cfg(not(feature = "ui-probe"))]
        let _ = key;
    }
}

/// The Bevy backend's root: the camera it draws to, the faces it draws
/// with, and the UI scale (`UiMetrics`, the one egui lays out in too) —
/// once any screen is on. Without
/// it the host has no root and every operation waits.
#[cfg(feature = "ui-probe")]
pub fn prepare_bevy(
    _main: bevy::ecs::system::NonSendMarker,
    lib: bevy::prelude::Res<crate::systems::lib_settings::LibScreens>,
    cameras: bevy::prelude::Query<
        bevy::prelude::Entity,
        bevy::prelude::With<crate::components::FreeCamera>,
    >,
    targets: bevy::prelude::Query<&bevy::camera::RenderTarget>,
    mut fonts: bevy::prelude::ResMut<bevy::prelude::Assets<bevy::prelude::Font>>,
    mut ui_scale: bevy::prelude::ResMut<bevy::prelude::UiScale>,
    metrics: bevy::prelude::Res<crate::systems::ui_input::UiMetrics>,
    mut commands: bevy::prelude::Commands,
    mut ready: bevy::prelude::Local<bool>,
) {
    use crate::systems::{icons, theme};
    if *ready
        || !(lib.settings
            || lib.layers
            || lib.search
            || lib.route
            || lib.places
            || lib.place_card
            || lib.building_card
            || lib.chats
            || lib.person_card
            || lib.reminder
            || lib.filter_chip
            || lib.post_composer
            || lib.login
            || lib.welcome
            || lib.app_update
            || lib.avatar
            || lib.models
            || lib.script
            || lib.plans
            || lib.scenario
            || lib.design_room
            || lib.admin
            || lib.chrome
            || lib.osd
            || lib.pins
            || lib.place_names
            || lib.chat_overlay
            || lib.walk_guide
            || lib.interact
            || lib.attach_chip
            || lib.poi_card
            || lib.dialogs
            || lib.main_menu
            || lib.share_sheet
            || lib.action_ring
            || lib.satellite_menu
            || lib.locate
            || lib.driver
            || lib.camera
            || lib.call
            || lib.cosmos
            || lib.splash
            || lib.face_viewfinder
            || lib.dev_panel
            || lib.picture_viewer
            || lib.mcp_activity
            || lib.midas_link
            || lib.release_approval
            || lib.object_editor
            || lib.marquee)
    {
        return;
    }
    let Ok(camera) = cameras.single() else {
        return;
    };
    ui_scale.0 = metrics.ui_scale();
    let body = fonts.add(bevy::prelude::Font::from_bytes(theme::BODY_TTF.to_vec()));
    // A camera of the library's own, drawing nothing but the library and
    // after the world's camera has finished — egui included, which draws
    // after the upscale, past where `bevy_ui`'s pass sits. On the world's
    // camera every egui button lay over the library's cards. It draws to
    // the world camera's target (`follow_world_target`).
    //
    // Unlike the world's (HDR, its own MSAA) it has a texture of its own,
    // laid over the target after: so it clears that texture to transparent
    // each frame and lays it over premultiplied. Left uncleared, every
    // frame drew its shadows onto the last's, and a 10 % shadow came out
    // a black ring.
    let target = targets.get(camera).cloned().unwrap_or_default();
    let overlay = commands
        .spawn((
            bevy::prelude::Camera2d,
            bevy::prelude::Camera {
                order: 100,
                clear_color: bevy::camera::ClearColorConfig::Custom(bevy::prelude::Color::NONE),
                output_mode: bevy::camera::CameraOutputMode::Write {
                    blend_state: Some(
                        bevy::render::render_resource::BlendState::PREMULTIPLIED_ALPHA_BLENDING,
                    ),
                    clear_color: bevy::camera::ClearColorConfig::None,
                },
                ..Default::default()
            },
            bevy::prelude::Msaa::Off,
            target,
            LibCamera,
        ))
        .id();
    commands.insert_resource(mithril_bevy::UiCamera(overlay));
    commands.insert_resource(mithril_bevy::FontMap {
        regular: body.clone(),
        semibold: fonts.add(bevy::prelude::Font::from_bytes(theme::BRAND_TTF.to_vec())),
        // egui's own monospace (Hack, in its default fonts), which egui's
        // code fields set; the body face before, so code was not monospace.
        mono: egui::FontDefinitions::default()
            .font_data
            .get("Hack")
            .map(|d| fonts.add(bevy::prelude::Font::from_bytes(d.font.to_vec())))
            .unwrap_or(body),
        icons: Some(fonts.add(bevy::prelude::Font::from_bytes(icons::FONT.to_vec()))),
        icon_scale: theme::ICON_SCALE,
    });
    *ready = true;
}

/// The library's own camera (`prepare_bevy`).
#[cfg(feature = "ui-probe")]
#[derive(bevy::prelude::Component)]
pub struct LibCamera;

/// The library's camera draws where the world's does: a capture moves the
/// world's camera onto an image (`shot_harness`), and the library has to
/// land in the same picture.
///
/// And it stands down while a picture of the map is taken
/// (`ScreenshotState::hide_ui`: a chat snapshot, a post's polaroid, your
/// face's), as egui does: drawn on its own camera, every library screen
/// open then was in the picture.
#[cfg(feature = "ui-probe")]
pub fn hide_for_snapshots(
    shots: Option<bevy::prelude::Res<crate::resources::ScreenshotState>>,
    mut lib: bevy::prelude::Query<&mut bevy::prelude::Camera, bevy::prelude::With<LibCamera>>,
) {
    let hide = shots.is_some_and(|s| s.hide_ui);
    for mut camera in &mut lib {
        if camera.is_active == hide {
            camera.is_active = !hide;
        }
    }
}

#[cfg(feature = "ui-probe")]
pub fn follow_world_target(
    world: bevy::prelude::Query<
        &bevy::camera::RenderTarget,
        (
            bevy::prelude::With<crate::components::FreeCamera>,
            bevy::prelude::Without<LibCamera>,
            bevy::prelude::Changed<bevy::camera::RenderTarget>,
        ),
    >,
    mut lib: bevy::prelude::Query<&mut bevy::camera::RenderTarget, bevy::prelude::With<LibCamera>>,
) {
    let (Ok(world), Ok(mut lib)) = (world.single(), lib.single_mut()) else {
        return;
    };
    *lib = world.clone();
}

/// The library's screens hold the input they are under, as egui's panels
/// do: a world click under any of them yields (`UiInput::owns_click`), and
/// a text field with the keyboard holds the game keys off
/// (`UiInput::ui_owns_keyboard`). Written into `UiInput` for the next frame;
/// `egui_manager::publish_ui_input` hands it on to the egui systems that
/// still ask `EguiManager`. As DOM the screens sit over the canvas, whose
/// pointer and keyboard Bevy reads, so only the Bevy backend needs telling.
#[cfg(feature = "ui-probe")]
pub fn bridge_input(
    focus: Option<bevy::prelude::Res<bevy::input_focus::InputFocus>>,
    fields: bevy::prelude::Query<(), bevy::prelude::With<bevy::text::EditableText>>,
    mut pointer: bevy::prelude::ResMut<mithril_bevy::UiPointer>,
    live: bevy::prelude::Query<(), bevy::prelude::With<bevy::ui::Node>>,
    mut input: bevy::prelude::ResMut<crate::systems::ui_input::UiInput>,
) {
    let owns = focus
        .and_then(|f| f.get())
        .is_some_and(|e| fields.contains(e));
    if owns != input.overlay_owns_keyboard() {
        bevy::log::debug!(owns, "lib_host: a library field has the keyboard");
    }
    // A node despawned under the pointer (a card closed by its ✕) never
    // sees the pointer leave, and would hold every world click after it.
    if pointer.over.iter().any(|e| !live.contains(*e)) {
        pointer.over.retain(|e| live.contains(*e));
    }
    let pointer = pointer.owns_pointer();
    if input.overlay_owns_pointer() != pointer || input.overlay_owns_keyboard() != owns {
        input.set_overlay(pointer, owns);
    }
}

/// The map's free area, from the library's own layout: the screen less the
/// box of every element marked `data-reserves-map` (`ui_input::RESERVES_MAP`
/// — the side panels and the Appearance sheet), each cut from the edge it
/// names (`ui_input::free_map`). Written into `UiInput::map_rect` in window
/// points, for the next frame's dock, search bar, round buttons, pins and
/// names. `PostUpdate`, after `bevy_ui`'s layout, so the boxes are this
/// frame's.
#[cfg(feature = "ui-probe")]
pub fn measure_map_room(world: &mut bevy::prelude::World) {
    use bevy::math::{Rect, Vec2};

    use crate::systems::ui_input::{free_map, MapSide, UiInput, UiMetrics, RESERVES_MAP};

    if !world.contains_resource::<mithril_bevy::UiTree>() {
        return;
    }
    let rooms: Vec<(MapSide, Rect)> = mithril_bevy::marked_rects(world, RESERVES_MAP)
        .into_iter()
        .filter_map(|(side, rect)| Some((MapSide::parse(&side)?, rect)))
        .collect();
    let metrics = world.resource::<UiMetrics>();
    let k = metrics.ui_scale();
    let map = free_map(
        Rect::from_corners(Vec2::ZERO, metrics.viewport_pts()),
        rooms,
    );
    let window = Rect::from_corners(map.min * k, map.max * k);
    let mut input = world.resource_mut::<UiInput>();
    if input.map_rect() != Some(window) {
        input.set_map_rect(Some(window));
    }
}

/// [`measure_map_room`] on the web: the marked elements' boxes as the
/// browser laid them out (`getBoundingClientRect`), mapped from the screens'
/// container onto the window. `PreUpdate`, reading the layout the browser
/// made for the last frame, so nothing forces a reflow mid-frame.
#[cfg(all(feature = "ui-dom", target_arch = "wasm32"))]
pub fn measure_map_room_dom(
    metrics: bevy::prelude::Res<crate::systems::ui_input::UiMetrics>,
    mut input: bevy::prelude::ResMut<crate::systems::ui_input::UiInput>,
) {
    let window = metrics.viewport_pts() * metrics.ui_scale();
    let Some(map) = dom::map_room(window) else {
        return;
    };
    if input.map_rect() != Some(map) {
        input.set_map_rect(Some(map));
    }
}

#[cfg(all(feature = "ui-dom", target_arch = "wasm32"))]
mod dom {
    use std::cell::RefCell;

    use mithril::dom::App;
    use mithril::view::View;
    use wasm_bindgen::JsCast;

    use crate::systems::theme;

    /// The container every screen mounts into, over the canvas.
    const CONTAINER: &str = "cartopolis-ui";
    /// Our stylesheet's element.
    const STYLE: &str = "cartopolis-ui-css";

    thread_local! {
        static KEYED: RefCell<Vec<(String, App)>> = const { RefCell::new(Vec::new()) };
    }

    fn document() -> Option<web_sys::Document> {
        web_sys::window().and_then(|w| w.document())
    }

    /// The container and the stylesheet, made once.
    fn ensure_page() -> Option<web_sys::Element> {
        let document = document()?;
        let body = document.body()?;
        if document.get_element_by_id(STYLE).is_none() {
            let style = document.create_element("style").ok()?;
            style.set_id(STYLE);
            let tokens = theme::app_tokens();
            let mut css = String::new();
            for (name, sheet) in mithril::style::sheets(&tokens) {
                // The page's own baseline stays the page's.
                if name.ends_with("base.css") {
                    continue;
                }
                css.push_str(&sheet);
                css.push('\n');
            }
            css.push_str(&format!(
                "#{CONTAINER} {{ position: absolute; inset: 0; z-index: 10; pointer-events: none; \
                 font-family: var(--m-font); font-size: var(--m-font-size); color: var(--m-text); }}\n\
                 #{CONTAINER} > * {{ pointer-events: auto; }}\n"
            ));
            style.set_text_content(Some(&css));
            // Into the body, not the head: `Document::head` is a web-sys
            // feature the client does not carry, and a stylesheet applies
            // from either.
            body.append_child(&style).ok()?;
        }
        match document.get_element_by_id(CONTAINER) {
            Some(c) => Some(c),
            None => {
                let container = document.create_element("div").ok()?;
                container.set_id(CONTAINER);
                body.append_child(&container).ok()?;
                Some(container)
            }
        }
    }

    pub fn mount_keyed(key: &str, view: View, dark: bool, coarse_pointer: bool) {
        clear_keyed(key);
        if ensure_page().is_none() {
            return;
        }
        let _ = coarse_pointer;
        // The container's own theme, not the page's: the frame's theme
        // follows the app's setting (`ui_theme`), the site's its own.
        if let Some(c) = document().and_then(|d| d.get_element_by_id(CONTAINER)) {
            let _ = c.set_attribute("data-theme", if dark { "dark" } else { "light" });
        }
        let app = mithril::dom::mount_to(CONTAINER, view);
        KEYED.with(|k| k.borrow_mut().push((key.to_owned(), app)));
    }

    pub fn clear_keyed(key: &str) {
        let gone: Vec<App> = KEYED.with(|k| {
            let mut k = k.borrow_mut();
            let (gone, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut *k)
                .into_iter()
                .partition(|(name, _)| name == key);
            *k = kept;
            gone.into_iter().map(|(_, app)| app).collect()
        });
        unmount(gone);
    }

    fn unmount(apps: Vec<App>) {
        let Some(parent) = document().and_then(|d| d.get_element_by_id(CONTAINER)) else {
            return;
        };
        let parent: web_sys::Node = parent.unchecked_into();
        for app in apps {
            app.mounted.unmount(&app.renderer, &parent);
        }
    }

    /// The map in window points (`window` its size): the container less
    /// every `[data-reserves-map]` element's box, each mapped from the
    /// container's CSS pixels onto the window — on the phone tier the canvas
    /// is scaled by the render budget's stage transform, so a CSS pixel is
    /// not a window point. `None` before anything has mounted.
    pub fn map_room(window: bevy::math::Vec2) -> Option<bevy::math::Rect> {
        use bevy::math::{Rect, Vec2};
        use js_sys::Reflect;
        use wasm_bindgen::JsValue;

        use crate::systems::ui_input::{free_map, MapSide, RESERVES_MAP};

        let container = document()?.get_element_by_id(CONTAINER)?;
        let frame = client_rect(&container)?;
        if frame.width() <= 0.0 || frame.height() <= 0.0 {
            return None;
        }
        let k = Vec2::new(window.x / frame.width(), window.y / frame.height());
        let to_window =
            |r: Rect| Rect::from_corners((r.min - frame.min) * k, (r.max - frame.min) * k);
        let select = Reflect::get(&container, &JsValue::from_str("querySelectorAll"))
            .ok()?
            .dyn_into::<js_sys::Function>()
            .ok()?;
        let list = select
            .call1(&container, &JsValue::from_str(&format!("[{RESERVES_MAP}]")))
            .ok()?;
        let len = Reflect::get(&list, &JsValue::from_str("length"))
            .ok()?
            .as_f64()? as u32;
        let mut rooms = Vec::new();
        for i in 0..len {
            let Ok(el) = Reflect::get_u32(&list, i) else {
                continue;
            };
            let el: web_sys::Element = el.unchecked_into();
            let Some(side) = el
                .get_attribute(RESERVES_MAP)
                .and_then(|v| MapSide::parse(&v))
            else {
                continue;
            };
            let Some(r) = client_rect(&el) else {
                continue;
            };
            if r.width() > 0.0 && r.height() > 0.0 {
                rooms.push((side, to_window(r)));
            }
        }
        Some(free_map(Rect::from_corners(Vec2::ZERO, window), rooms))
    }

    /// `getBoundingClientRect()`, in CSS pixels, through `Reflect` (the
    /// client's web-sys carries no `DomRect`).
    fn client_rect(el: &wasm_bindgen::JsValue) -> Option<bevy::math::Rect> {
        use js_sys::Reflect;
        use wasm_bindgen::JsValue;

        let rect = Reflect::get(el, &JsValue::from_str("getBoundingClientRect"))
            .ok()?
            .dyn_into::<js_sys::Function>()
            .ok()?
            .call0(el)
            .ok()?;
        let at = |name: &str| -> Option<f32> {
            Reflect::get(&rect, &JsValue::from_str(name))
                .ok()?
                .as_f64()
                .map(|v| v as f32)
        };
        Some(bevy::math::Rect::new(
            at("left")?,
            at("top")?,
            at("right")?,
            at("bottom")?,
        ))
    }
}
