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
/// with, and the UI scale egui lays out in — once any screen is on. Without
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
    egui: crate::systems::egui_manager::EguiRef,
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
            || lib.keyboard
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
            || lib.driver)
    {
        return;
    }
    let Ok(camera) = cameras.single() else {
        return;
    };
    ui_scale.0 = egui.ui_scale();
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
/// do: a world click under any of them yields (`EguiManager::owns_click`),
/// and a text field with the keyboard holds the game keys off
/// (`ui_owns_keyboard`). As DOM the screens sit over the canvas, whose
/// pointer and keyboard Bevy reads, so only the Bevy backend needs telling.
#[cfg(feature = "ui-probe")]
pub fn bridge_input(
    focus: Option<bevy::prelude::Res<bevy::input_focus::InputFocus>>,
    fields: bevy::prelude::Query<(), bevy::prelude::With<bevy::text::EditableText>>,
    mut pointer: bevy::prelude::ResMut<mithril_bevy::UiPointer>,
    live: bevy::prelude::Query<(), bevy::prelude::With<bevy::ui::Node>>,
    mut egui: crate::systems::egui_manager::EguiMut,
) {
    let owns = focus
        .and_then(|f| f.get())
        .is_some_and(|e| fields.contains(e));
    if owns != egui.overlay_owns_keyboard() {
        bevy::log::debug!(owns, "lib_host: a library field has the keyboard");
    }
    egui.set_overlay_owns_keyboard(owns);
    // A node despawned under the pointer (a card closed by its ✕) never
    // sees the pointer leave, and would hold every world click after it.
    if pointer.over.iter().any(|e| !live.contains(*e)) {
        pointer.over.retain(|e| live.contains(*e));
    }
    egui.set_overlay_owns_pointer(pointer.owns_pointer());
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
}
