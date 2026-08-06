//! Prim UI system (Phase 5 - ADR-017).
//!
//! Renders context menus and edit dialogs using egui.

use crate::components::{NeedsMeshRebuild, Prim, PrimShape, Region, Selected};
use crate::resources::OnlineSession;
use crate::resources::RightPanel;
use crate::resources::{
    ContextMenuState, EditDialogState, GameState, PrimTextureCache, TextureLibrary,
};
use crate::systems::egui_manager::EguiManager;
use crate::systems::theme;
use bevy::pbr::StandardMaterial;
use bevy::prelude::*;
use egui::Window;
use viberfox_core::NetMessage;
use web_time::Instant;

/// Where the map controls (compass, locate) stack, clear of the logo card that
/// now owns the top-left corner. Read by `hud::render_map_controls`; one constant
/// because the compass and the locate button under it have to agree.
pub const MAP_CONTROLS_TOP: f32 = 52.0;

/// The address bar: the app mark, Back / Forward / Home, the geo search field,
/// and a hamburger holding what used to be the `File` menu.
///
/// **The logo is the switch, and it is the only part that is always there.** The
/// bar started as a full-width strip, which put an opaque 46 pt band across the
/// top of the map to carry ~400 pt of controls; then as a card that revealed
/// itself when the pointer grazed the top edge, which is undiscoverable (you
/// cannot aim at something you have never seen) and fired on every trip to the
/// window's title bar. Now a single mark sits in the corner, clicking it unfolds
/// the rest of the card to the right of it, and clicking it again folds it back —
/// the logo does not move between the two states, so the card reads as growing
/// out of it rather than appearing next to it. `Ctrl+L` (the browser shortcut)
/// unfolds it and focuses the field. The one thing that takes the card away is a
/// screen with no map on it: driver mode, or a right-hand panel that has gone
/// full-width on a compact viewport (see the body).
///
/// The panel toggles that used to live here (AI, Layers, Places, Appearance) and
/// the camera-mode picker live in the bottom bar ([`camera_picker_ui`],
/// `chat::render_chat_panel`), so this card is navigation only.
#[allow(clippy::too_many_arguments)]
pub fn render_menu_bar(
    mut egui: ResMut<EguiManager>,
    mut dev_state: ResMut<crate::resources::DevPanelState>,
    mut nav: ResMut<crate::systems::geo_nav::GeoNavState>,
    store: Res<crate::systems::user_store::UserStore>,
    keyboard: Res<ButtonInput<KeyCode>>,
    driver: Res<crate::systems::driver::DriverState>,
    mut right_panel: ResMut<crate::resources::RightPanel>,
    mut camera_state: ResMut<crate::resources::CameraState>,
    mut app_exit: MessageWriter<AppExit>,
    // For the "how far away is this hit" line in the suggestion rows: the same
    // camera fix the HUD reads out.
    globe: Res<crate::systems::globe::GlobeState>,
    camera: Query<(&big_space::prelude::GridCell, &Transform), With<crate::components::FreeCamera>>,
    // Follow mode needs an avatar to follow (logged-in viewer only).
    avatars: Query<(), With<crate::components::Avatar>>,
    // Whether the bar is unfolded, and the decoded logo. Both survive across
    // frames; the texture is uploaded once, on the first frame that draws.
    mut open: Local<bool>,
    mut logo: Local<Option<egui::TextureHandle>>,
    map_anchor: Res<crate::resources::MapAnchor>,
) {
    // Driving takes the whole screen (bare basemap, manoeuvre card in this very
    // corner) — the address bar is not part of that chrome, mark included.
    if driver.active {
        nav.search_has_focus = false;
        return;
    }
    // On a compact viewport a right-hand panel takes the *whole* screen
    // (`responsive_right_panel`), and this card is a floating `Area`, which egui
    // always paints above panels — so the mark sat on top of the panel's own
    // header with no map left behind it to navigate. Dropping it while a panel
    // owns the screen is the same workaround the compass and the status HUD use
    // (`hud::render_map_controls`); there is no z-order that puts an `Area`
    // behind a panel. On a wide viewport the panel docks to the right and never
    // reaches this corner, so the card stays. `*open` is left alone: closing the
    // panel brings the bar back in the state it was folded away in.
    if egui.compact() && !right_panel.is(RightPanel::None) {
        nav.search_has_focus = false;
        return;
    }
    let ctx = egui.ctx_mut();

    // Ctrl+L (browser address-bar shortcut): unfold the bar and grab the search
    // field's focus next frame (`nav.focus_search`, consumed in `nav_bar_ui`).
    let ctrl = keyboard.pressed(KeyCode::ControlLeft) || keyboard.pressed(KeyCode::ControlRight);
    if ctrl && keyboard.just_pressed(KeyCode::KeyL) {
        nav.focus_search = true;
    }
    // `focus_search` is also how a scripted run asks for the bar
    // (`geo_nav::seed_search`, `--ui search`), which has no pointer to click with.
    if nav.focus_search {
        *open = true;
    }
    if !*open {
        // No field on screen → it can hold no focus, and no dropdown hangs off it.
        nav.search_has_focus = false;
    }

    if logo.is_none() {
        *logo = theme::logo_image()
            .map(|img| ctx.load_texture("viberfox_logo", img, egui::TextureOptions::LINEAR));
    }

    // Where the camera is on Earth, for the suggestion rows' distances.
    let viewer = camera
        .single()
        .ok()
        .zip(map_anchor.geo)
        .map(|((cell, tf), anchor)| crate::systems::geo_nav::camera_geo(anchor, &globe, cell, tf));

    let mut toggle = false;
    // A floating `Area` (not a `Panel::top`) so the side panels keep their full
    // height and the card overlays them when it unfolds.
    egui::Area::new(egui::Id::new("menu_bar"))
        .order(egui::Order::Foreground)
        .fixed_pos(egui::pos2(8.0, 8.0))
        .show(ctx, |ui| {
            // `Frame::popup` rather than `theme::overlay_frame`: the shared overlay
            // frame is translucent, which is right for a HUD readout and wrong behind
            // a text field the user is typing into over a moving map. Folded, the card
            // keeps the *margins* and drops the paint — the logo carries its own teal
            // card, so a second one around it only muddies the mark, and matching the
            // margins is what keeps the logo from jumping when the bar unfolds.
            let frame = if *open {
                egui::Frame::popup(ui.style())
            } else {
                egui::Frame::NONE
            };
            frame
                .inner_margin(egui::Margin::symmetric(8, 6))
                .show(ui, |ui| {
                    // A plain row, **not** `egui::MenuBar`: `MenuBar::ui` ends with
                    // `set_min_size(available_width, …)`, which is right for a bar spanning a
                    // window and exactly wrong for a card that is supposed to size to its
                    // content — it stretched the card ~230 pt past the last button. Its other
                    // job, sharing a menu config so hovering slides between open menus, has
                    // nothing left to do here now that there is one menu.
                    //
                    // `interact_size.y` is the row height and `horizontal` centres against it,
                    // so bumping it gives the buttons and the taller search field a common,
                    // roomier height that lines up.
                    ui.style_mut().spacing.interact_size.y = 26.0;
                    ui.style_mut().spacing.item_spacing.x = 6.0;
                    ui.horizontal(|ui| {
                        // The mark, and the switch. Frameless: the asset is already a rounded
                        // teal card, so egui's button frame would draw a box around a box.
                        let mark = match logo.as_ref() {
                            // A `SizedTexture` straight off the handle, not an `Image` from a
                            // URI: this bypasses egui's image *loader* machinery entirely
                            // (`egui_extras` is not a dependency), because the texture is
                            // already uploaded.
                            Some(tex) => ui.add(
                                egui::Button::image(egui::load::SizedTexture::new(
                                    tex.id(),
                                    egui::Vec2::splat(theme::LOGO_SIZE),
                                ))
                                .frame(false),
                            ),
                            // No logo means a checkout without Git LFS. Say so in the tooltip
                            // rather than leaving a dead corner.
                            None => ui.add(
                                egui::Button::new(egui::RichText::new("V").heading())
                                    .frame(false)
                                    .min_size(egui::Vec2::splat(theme::LOGO_SIZE)),
                            ),
                        };
                        if mark
                            .on_hover_text(if *open {
                                "Hide the address bar"
                            } else {
                                "Search and navigate"
                            })
                            .clicked()
                        {
                            toggle = true;
                        }
                        if !*open {
                            return;
                        }
                        // Browser-style Back / Forward through the location history. Every
                        // teleport (search, bookmark, Home, coords) is a history entry.
                        if ui
                            .add_enabled(nav.can_go_back(), egui::Button::new("◀"))
                            .on_hover_text("Back to the previous location")
                            .on_disabled_hover_text("No previous location")
                            .clicked()
                        {
                            nav.go_back();
                        }
                        if ui
                            .add_enabled(nav.can_go_forward(), egui::Button::new("▶"))
                            .on_hover_text("Forward to the next location")
                            .on_disabled_hover_text("No next location")
                            .clicked()
                        {
                            nav.go_forward();
                        }
                        // Home button, browser-style, beside the address/search bar: jump to
                        // the user's saved Home location (set via the status panel's 🏠
                        // button). Disabled until a Home has been saved.
                        let home = store.data.home.clone();
                        let resp = ui
                            .add_enabled(home.is_some(), egui::Button::new("🏠"))
                            .on_hover_text("Go to your Home location")
                            .on_disabled_hover_text(
                                "No Home set — use the 🏠 button in the status panel",
                            );
                        if resp.clicked() {
                            if let Some(h) = &home {
                                nav.pending_teleport = Some(crate::systems::geo_nav::Teleport {
                                    lat: h.lat,
                                    lng: h.lng,
                                    altitude: Some(h.altitude),
                                    pose: Some(h.pose.clone()),
                                });
                            }
                        }
                        // Browser-style geo search: place names via Nominatim, or pasted
                        // "lat, lng(, altitude)" coordinates. The route planner (ADR-037) is
                        // reached from the always-visible bottom bar (🧭 Route), alongside the
                        // other right-hand panels, rather than this card.
                        crate::systems::geo_nav::nav_bar_ui(ui, &mut nav, viewer);

                        // Camera mode, beside the address it applies to — "where am I looking
                        // from" is the same question as "where am I". It sat in the bottom bar
                        // while this strip auto-hid and could not be relied on.
                        camera_picker_ui(ui, &mut camera_state, !avatars.is_empty(), &globe);

                        // The app menu, at the far end where a browser puts it. Probed rather
                        // than hard-coded: `☰` is one of the few symbols the loaded font
                        // actually has, but that is a fact about the font stack, not a
                        // guarantee (see `theme::first_drawable`).
                        let hamburger = theme::first_drawable(ui.ctx(), &["☰", "≡", "⋮", "="]);
                        ui.menu_button(hamburger, |ui| {
                            // Panels you *visit* rather than flip: Appearance is a wardrobe and
                            // the Assistant is a conversation, so neither earns a permanent
                            // disc in the dock next to the four places you actually navigate
                            // between.
                            for panel in [
                                crate::resources::RightPanel::Appearance,
                                crate::resources::RightPanel::Assistant,
                            ] {
                                // Same glyph the panel's own header wears, so the menu entry
                                // and the badge it opens are recognisably one thing.
                                let icon = theme::first_drawable(ui.ctx(), panel.icons());
                                if ui
                                    .selectable_label(
                                        right_panel.is(panel),
                                        format!("{icon}  {}", panel.label()),
                                    )
                                    .clicked()
                                {
                                    right_panel.toggle(panel);
                                    ui.close();
                                }
                            }
                            ui.separator();
                            let dev_label = if dev_state.open {
                                "Hide developer tools"
                            } else {
                                "Developer tools"
                            };
                            if ui.button(dev_label).clicked() {
                                dev_state.open = !dev_state.open;
                                ui.close();
                            }
                            ui.separator();
                            if ui.button("Exit").clicked() {
                                app_exit.write(AppExit::Success);
                                ui.close();
                            }
                        })
                        .response
                        .on_hover_text("Menu");
                    });
                });
        });
    if toggle {
        *open = !*open;
        // Folding it away drops the dropdown with it, or the suggestions would
        // hang under a bar that is no longer there.
        if !*open {
            nav.close_suggestions();
        }
    }
}

/// Camera-mode picker (Follow / Fly / Earth, plus a one-click "View Whole
/// Earth"), drawn in the address bar beside the search field. The mode is also
/// Tab-cyclable. Follow needs an avatar (logged-in viewer only).
pub fn camera_picker_ui(
    ui: &mut egui::Ui,
    camera_state: &mut crate::resources::CameraState,
    has_avatar: bool,
    globe: &crate::systems::globe::GlobeState,
) {
    use crate::resources::CameraMode;
    let label = match camera_state.mode {
        CameraMode::Avatar => "📷 Follow",
        CameraMode::Free => "📷 Fly",
        CameraMode::Orbit => "📷 Earth",
    };
    ui.menu_button(label, |ui| {
        let mut pick: Option<CameraMode> = None;
        for (mode, name) in [
            (CameraMode::Avatar, "Follow (third-person)"),
            (CameraMode::Free, "Fly (free camera)"),
            (CameraMode::Orbit, "Earth (orbit the globe)"),
        ] {
            let enabled = has_avatar || mode != CameraMode::Avatar;
            let mut clicked = false;
            ui.add_enabled_ui(enabled, |ui| {
                clicked = ui
                    .selectable_label(camera_state.mode == mode, name)
                    .clicked();
            });
            if clicked {
                pick = Some(mode);
            }
        }
        // One-click jump to an orbit that frames the whole planet.
        let mut whole_earth = false;
        if globe.ready {
            ui.separator();
            if ui.button("🌍 View Whole Earth").clicked() {
                whole_earth = true;
            }
        }
        if let Some(mode) = pick {
            // Same entry behaviour as the Tab cycle (restore/seed per mode).
            crate::systems::free_camera::enter_mode(camera_state, mode);
            camera_state.mode = mode;
            ui.close();
        }
        if whole_earth {
            use bevy::math::DVec3;
            let cs = &mut *camera_state;
            cs.mode = CameraMode::Orbit;
            cs.orbit_initialized = true;
            cs.orbit_has_pose = true;
            cs.orbit_space = true;
            cs.orbit_focus = DVec3::ZERO;
            cs.orbit_distance = globe.radius as f64 * 3.0;
            cs.orbit_distance_target = cs.orbit_distance;
            // Over the anchor, slightly north-east; heading 0 so the view arrives
            // north-up (free_camera derives the up vector from the planet's north
            // axis and this heading).
            cs.orbit_dir = DVec3::new(0.54, 0.30, 0.79).normalize();
            cs.orbit_heading = 0.0;
            ui.close();
        }
    });
}

/// Toggle the AI panel with Ctrl+Shift+A.
pub fn toggle_ai_panel_shortcut(
    keyboard: Res<ButtonInput<KeyCode>>,
    mut right_panel: ResMut<RightPanel>,
) {
    let ctrl = keyboard.pressed(KeyCode::ControlLeft) || keyboard.pressed(KeyCode::ControlRight);
    let shift = keyboard.pressed(KeyCode::ShiftLeft) || keyboard.pressed(KeyCode::ShiftRight);
    if ctrl && shift && keyboard.just_pressed(KeyCode::KeyA) {
        right_panel.toggle(RightPanel::Assistant);
    }
}

/// Toggle the developer panel with Ctrl+Shift+D.
pub fn toggle_dev_panel_shortcut(
    keyboard: Res<ButtonInput<KeyCode>>,
    mut dev_state: ResMut<crate::resources::DevPanelState>,
) {
    let ctrl = keyboard.pressed(KeyCode::ControlLeft) || keyboard.pressed(KeyCode::ControlRight);
    let shift = keyboard.pressed(KeyCode::ShiftLeft) || keyboard.pressed(KeyCode::ShiftRight);
    if ctrl && shift && keyboard.just_pressed(KeyCode::KeyD) {
        dev_state.open = !dev_state.open;
    }
}

/// Developer panel (View ▸ Dev Panel or Ctrl+Shift+D): a day/night slider that drives
/// the sun position so the lighting can be tested at any time of day.
#[allow(clippy::too_many_arguments)]
pub fn render_dev_panel(
    mut egui: ResMut<EguiManager>,
    mut dev: ResMut<crate::resources::DevPanelState>,
    mut weather: ResMut<crate::systems::weather::WeatherState>,
    mut shot: ResMut<crate::resources::ScreenshotState>,
    // For "📋 Copy as preset": the same camera fix the HUD reads.
    globe: Res<crate::systems::globe::GlobeState>,
    camera: Query<(&big_space::prelude::GridCell, &Transform), With<crate::components::FreeCamera>>,
    mut copied_at: Local<Option<Instant>>,
    map_anchor: Res<crate::resources::MapAnchor>,
) {
    if !dev.open {
        return;
    }
    let ctx = egui.ctx_mut();
    let mut open = true;
    egui::Window::new("Dev — Lighting")
        .open(&mut open)
        .default_width(260.0)
        .show(ctx, |ui| {
            // ── Day / night cycle ──────────────────────────────────────────
            // Auto tracks the real local time (sun *and* moon); Day/Night force
            // noon/midnight; Manual unlocks the controls below. The manual
            // controls are disabled outside Manual — like the weather sliders
            // under Auto, they read the automatic value but can't be moved.
            ui.label("Day / night cycle");
            ui.horizontal(|ui| {
                use crate::resources::SunMode;
                ui.selectable_value(&mut dev.sun_mode, SunMode::Auto, "Auto")
                    .on_hover_text("Sun and moon follow the real local time");
                ui.selectable_value(&mut dev.sun_mode, SunMode::Day, "Day");
                ui.selectable_value(&mut dev.sun_mode, SunMode::Night, "Night");
                ui.selectable_value(&mut dev.sun_mode, SunMode::Manual, "Manual");
            });
            let manual = dev.sun_mode == crate::resources::SunMode::Manual;
            ui.add_enabled_ui(manual, |ui| {
                ui.add(
                    egui::Slider::new(&mut dev.time_of_day, 0.0..=24.0)
                        .suffix(" h")
                        .step_by(0.1),
                );
                let h = dev.time_of_day.floor() as i32 % 24;
                let m = (dev.time_of_day.fract() * 60.0).floor() as i32;
                ui.label(format!("Time: {:02}:{:02}", h, m));
                ui.horizontal(|ui| {
                    if ui.button("Sunrise").clicked() {
                        dev.time_of_day = 6.0;
                    }
                    if ui.button("Noon").clicked() {
                        dev.time_of_day = 12.0;
                    }
                    if ui.button("Sunset").clicked() {
                        dev.time_of_day = 18.0;
                    }
                    if ui.button("Midnight").clicked() {
                        dev.time_of_day = 0.0;
                    }
                });
                ui.add(
                    egui::Slider::new(
                        &mut dev.moon_age_days,
                        0.0..=crate::utils::sun::LUNAR_CYCLE_DAYS,
                    )
                    .text("moon age (days)")
                    .step_by(0.1),
                );
                let lit = crate::utils::sun::moon_illuminated_fraction(dev.moon_age_days);
                ui.label(format!("{:.0}% lit (0 = new, ~14.8 = full)", lit * 100.0));
                ui.horizontal(|ui| {
                    crate::systems::theme::checkbox(ui, &mut dev.auto_cycle, "Quick cycle");
                    ui.add(
                        egui::DragValue::new(&mut dev.cycle_seconds)
                            .suffix(" s")
                            .range(1.0..=120.0)
                            .speed(0.5),
                    );
                });
            });
            ui.separator();

            // ── Weather ────────────────────────────────────────────────────
            // Same shape as the day/night group: Auto pulls the live met.no
            // forecast for the map anchor (driving cloud, rain, and fog);
            // Manual unlocks the sliders. `weather.live` is the Auto flag.
            ui.label("Weather");
            ui.horizontal(|ui| {
                ui.selectable_value(&mut weather.live, true, "Auto")
                    .on_hover_text("Live conditions for the map anchor from api.met.no");
                ui.selectable_value(&mut weather.live, false, "Manual");
            });
            if weather.live {
                if let Some(o) = weather.latest {
                    ui.label(format!(
                        "{:.0}% cloud · {:.1} mm/h · {:.0}°C · {:.0} m/s wind",
                        o.cloud_cover * 100.0,
                        o.precipitation * crate::systems::weather::RAIN_FULL_MM,
                        o.air_temp_c,
                        o.wind_ms,
                    ))
                    .on_hover_text(format!("sampled at {:.4}, {:.4}", o.lat, o.lng));
                } else {
                    ui.label("fetching…");
                }
            }
            ui.add_enabled_ui(!weather.live, |ui| {
                ui.add(
                    egui::Slider::new(&mut dev.cloud_cover, 0.0..=1.0)
                        .text("cloud cover")
                        .step_by(0.01),
                );
                ui.add(
                    egui::Slider::new(&mut dev.precipitation, 0.0..=1.0)
                        .text("rain")
                        .step_by(0.01),
                );
                ui.add(
                    egui::Slider::new(&mut dev.fog_density, 0.0..=1.0)
                        .text("fog density")
                        .step_by(0.01),
                );
            });
            ui.separator();
            ui.label("Render features");
            crate::systems::theme::checkbox(ui, &mut dev.shadows_enabled, "Shadows (sun + moon)");
            // Atmosphere / env-map IBL can't be hot-swapped — removing those
            // components at runtime hangs the GPU on this stack — so they apply at
            // startup only (a live flip needs a relaunch to take effect).
            crate::systems::theme::checkbox(
                ui,
                &mut dev.atmosphere_enabled,
                "Atmosphere (restart)",
            );
            ui.add_enabled(
                dev.atmosphere_enabled,
                egui::Checkbox::new(&mut dev.envmap_enabled, "Env-map IBL (restart)"),
            );
            ui.separator();
            crate::systems::theme::checkbox(ui, &mut dev.show_stats, "Show stats overlay (FPS)");
            ui.separator();
            ui.label("README screenshots");
            // The documentation set is generated from the remembered camera
            // positions in `docs/shots.toml` (`cargo shots`), not from this
            // button — but a *new* position has to be found by flying to it, so
            // this copies the current camera out as a pasteable `[[shot]]` block.
            // That is the whole handoff from "looks good here" to reproducible.
            ui.horizontal(|ui| {
                let fix = camera
                    .single()
                    .ok()
                    .zip(map_anchor.geo)
                    .map(|((cell, tf), anchor)| {
                        let (lat, lng) =
                            crate::systems::geo_nav::camera_geo(anchor, &globe, cell, tf);
                        // (yaw, pitch, roll) — the same decomposition free_camera
                        // uses to seed its look angles from the live view, so a
                        // pasted preset reproduces this exact framing.
                        let euler = tf.rotation.to_euler(EulerRot::YXZ);
                        (
                            lat,
                            lng,
                            globe.altitude_m(cell, tf),
                            euler.1.to_degrees(),
                            euler.0.to_degrees(),
                        )
                    });
                let copied = copied_at.is_some_and(|t| t.elapsed().as_secs_f32() < 1.5);
                if ui
                    .add_enabled(fix.is_some(), egui::Button::new("📋 Copy as preset"))
                    .on_hover_text("Copies this camera as a [[shot]] block for docs/shots.toml")
                    .clicked()
                {
                    if let Some((lat, lng, alt, pitch, yaw)) = fix {
                        crate::utils::clipboard::set_text(
                            &crate::systems::shot_presets::preset_toml(
                                "new-view",
                                lat,
                                lng,
                                alt,
                                pitch,
                                yaw,
                                dev.time_of_day,
                                dev.fog_density,
                                dev.cloud_cover,
                            ),
                        );
                        *copied_at = Some(Instant::now());
                    }
                }
                if copied {
                    ui.label("copied");
                }
            });
            ui.horizontal(|ui| {
                // Clicking captures a chrome-free shot into the next slot, cycling
                // docs/img/screenshot-{1..N}.png. The `capture_requested` guard stops
                // a held click from queuing more than one capture.
                if ui
                    .button(format!("📷 Capture slot {}", shot.next_slot + 1))
                    .on_hover_text(
                        "Hides the UI for one frame, then saves docs/img/screenshot-N.png",
                    )
                    .clicked()
                    && !shot.hide_ui
                {
                    shot.hide_ui = true;
                    shot.capture_requested = true;
                }
                if ui.button("Reset to 1").clicked() {
                    shot.next_slot = 0;
                }
            });
        });
    // Closing via the window's [x] only writes when it actually changed, to avoid
    // marking the resource changed every frame (which would re-run the lighting update).
    if !open && dev.open {
        dev.open = false;
    }
}

/// Avatar chooser (View ▸ Choose Avatar): pick body (Man/Woman) and clothing
/// theme. Edits a draft; Apply commits it to the persisted store, which
/// `avatar::respawn_avatar_on_appearance_change` watches to rebuild the model.
pub fn render_avatar_chooser(
    mut egui: ResMut<EguiManager>,
    mut chooser: ResMut<crate::resources::AvatarChooserState>,
    mut store: ResMut<crate::systems::user_store::UserStore>,
    mut right_panel: ResMut<RightPanel>,
) {
    use crate::systems::avatar::{accessory_styles_for, styles_for};
    use crate::systems::user_store::AvatarSex;
    let mut is_open = right_panel.is(RightPanel::Appearance);
    // Seed the draft from the saved appearance the first time the panel opens.
    if is_open && !chooser.seeded {
        chooser.draft = store.data.appearance.clone();
        chooser.seeded = true;
    }
    let saved = store.data.appearance.clone();
    let mut apply = false;
    let mut close = false;
    let compact = egui.compact();
    let viewport_w = egui.viewport_pts().x;
    crate::systems::egui_manager::responsive_right_panel(
        "appearance_panel",
        compact,
        viewport_w,
        280.0,
    )
    .show_collapsible(egui.ui_mut(), &mut is_open, |ui| {
        let panel = crate::resources::RightPanel::Appearance;
        close = theme::panel_header(ui, panel.icons(), panel.label());
        let d = &mut chooser.draft;
        ui.label("Body");
        ui.horizontal(|ui| {
            for sex in AvatarSex::ALL {
                if ui.selectable_label(d.sex == sex, sex.label()).clicked() {
                    d.sex = sex;
                    fix_styles_for_sex(d, sex);
                }
            }
        });
        let sex = d.sex;

        ui.add_space(6.0);
        // Preset: set all four clothing slots at once (quick whole-look pick).
        ui.label("Preset");
        let preset = d.uniform_style().unwrap_or("Custom").to_string();
        egui::ComboBox::from_id_salt("avatar_preset")
            .selected_text(preset)
            .show_ui(ui, |ui| {
                for style in styles_for(sex) {
                    if ui
                        .selectable_label(d.uniform_style() == Some(style), style)
                        .clicked()
                    {
                        d.head = style.to_string();
                        d.top = style.to_string();
                        d.legs = style.to_string();
                        d.feet = style.to_string();
                    }
                }
            });

        ui.separator();
        ui.label("Customize");
        slot_combo(ui, "Head", sex, &mut d.head);
        slot_combo(ui, "Top", sex, &mut d.top);
        slot_combo(ui, "Legs", sex, &mut d.legs);
        slot_combo(ui, "Shoes", sex, &mut d.feet);

        // Accessory (optional, non-weapon).
        egui::ComboBox::from_id_salt("avatar_accessory")
            .selected_text(format!(
                "Accessory: {}",
                d.accessory.as_deref().unwrap_or("None")
            ))
            .show_ui(ui, |ui| {
                if ui.selectable_label(d.accessory.is_none(), "None").clicked() {
                    d.accessory = None;
                }
                for acc in accessory_styles_for(sex) {
                    if ui
                        .selectable_label(d.accessory.as_deref() == Some(acc), acc)
                        .clicked()
                    {
                        d.accessory = Some(acc.to_string());
                    }
                }
            });

        ui.separator();
        let changed = *d != saved;
        ui.horizontal(|ui| {
            if ui
                .add_enabled(changed, egui::Button::new("Apply"))
                .clicked()
            {
                apply = true;
            }
            if !changed {
                ui.weak("This is your current avatar.");
            }
        });
    });
    if apply {
        store.data.appearance = chooser.draft.clone();
        store.dirty = true;
    }
    if close {
        *right_panel = RightPanel::None;
        chooser.seeded = false;
    }
}

/// One labelled per-slot style dropdown (Head / Top / Legs / Shoes).
fn slot_combo(
    ui: &mut egui::Ui,
    label: &str,
    sex: crate::systems::user_store::AvatarSex,
    value: &mut String,
) {
    egui::ComboBox::from_id_salt(label)
        .selected_text(format!("{label}: {value}"))
        .show_ui(ui, |ui| {
            for style in crate::systems::avatar::styles_for(sex) {
                ui.selectable_value(value, style.to_string(), style);
            }
        });
}

/// After a body (sex) change, reset any slot/accessory style not available for the
/// new sex to a valid one, so the mix never references a missing character.
fn fix_styles_for_sex(
    d: &mut crate::systems::user_store::AvatarAppearance,
    sex: crate::systems::user_store::AvatarSex,
) {
    let avail: Vec<&str> = crate::systems::avatar::styles_for(sex).collect();
    let first = avail.first().copied().unwrap_or("Casual").to_string();
    for slot in [&mut d.head, &mut d.top, &mut d.legs, &mut d.feet] {
        if !avail.contains(&slot.as_str()) {
            *slot = first.clone();
        }
    }
    if let Some(acc) = &d.accessory {
        if !crate::systems::avatar::accessory_styles_for(sex).any(|a| a == acc) {
            d.accessory = None;
        }
    }
}

/// Fires the pending README screenshot requested from the dev panel. Runs in Update
/// **after** `render_dev_panel` so the button click is visible this frame; because
/// `suppress_egui_for_screenshot` drops the egui output later this same frame (while
/// `hide_ui` is set), the captured swapchain has no UI chrome. After the capturing
/// frame, `hide_ui` is cleared and the panel reappears.
pub fn drive_screenshots(
    mut commands: Commands,
    mut shot: ResMut<crate::resources::ScreenshotState>,
) {
    use bevy::render::view::screenshot::Screenshot;

    if shot.capture_requested {
        // The `docs/img/` dir must exist or the save fails with an IO error.
        if let Err(e) = std::fs::create_dir_all("docs/img") {
            warn!("Could not create docs/img/ directory: {e}");
            shot.capture_requested = false;
            shot.hide_ui = false;
            return;
        }
        let slot = shot.next_slot;
        let path = std::path::PathBuf::from(format!("docs/img/screenshot-{}.png", slot + 1));
        commands
            .spawn(Screenshot::primary_window())
            .observe(save_screenshot_fixed_resolution(path));
        shot.next_slot = (slot + 1) % crate::resources::SCREENSHOT_SLOTS;
        shot.capture_requested = false;
        // Leave `hide_ui` set: this is the frame being captured, so the UI must stay
        // hidden. It's cleared on the next run below.
    } else if shot.hide_ui {
        shot.hide_ui = false;
    }
}

/// Screenshot observer (a fixed-resolution replacement for Bevy's `save_to_disk`):
/// normalizes every capture to `SCREENSHOT_WIDTH`×`SCREENSHOT_HEIGHT` so README images
/// stay consistent regardless of window size. The capture is center-cropped to the
/// target aspect ratio and resampled (`resize_to_fill`), then the alpha channel — which
/// carries HDR brightness, not transparency — is dropped before saving as RGB PNG.
fn save_screenshot_fixed_resolution(
    path: std::path::PathBuf,
) -> impl FnMut(On<bevy::render::view::screenshot::ScreenshotCaptured>) {
    use crate::resources::{SCREENSHOT_HEIGHT, SCREENSHOT_WIDTH};

    move |captured| {
        let dynamic = match captured.image.clone().try_into_dynamic() {
            Ok(img) => img,
            Err(e) => {
                error!("Screenshot capture could not be decoded: {e}");
                return;
            }
        };
        let normalized = dynamic
            .resize_to_fill(
                SCREENSHOT_WIDTH,
                SCREENSHOT_HEIGHT,
                image::imageops::FilterType::Lanczos3,
            )
            .to_rgb8();
        match normalized.save(&path) {
            Ok(()) => info!(
                "Screenshot saved to {} ({}×{})",
                path.display(),
                SCREENSHOT_WIDTH,
                SCREENSHOT_HEIGHT
            ),
            Err(e) => error!("Cannot save screenshot {}: {e}", path.display()),
        }
    }
}

/// When a screenshot is being captured, drop this frame's tessellated egui geometry so
/// the shot is free of UI chrome. Runs in PostUpdate **after** `end_egui_frame` (which
/// writes the output) and before render-world extraction. Only `clipped_primitives` is
/// cleared — `textures_delta` is left intact so egui's font/texture uploads aren't lost.
pub fn suppress_egui_for_screenshot(
    shot: Res<crate::resources::ScreenshotState>,
    mut output: ResMut<crate::systems::egui_manager::EguiRenderOutput>,
) {
    if shot.hide_ui {
        output.clipped_primitives.clear();
        // This mutates the output in place, so `end_egui_frame` must rewrite it next
        // frame even if egui reports nothing changed — otherwise the cleared geometry
        // is what stays resident and the UI never comes back after a screenshot.
        output.force_next_write = true;
        output.generation = output.generation.wrapping_add(1);
    }
}

/// Toggleable performance overlay (top-left, below the menu bar): smoothed FPS,
/// frame time, and live entity count. Enabled from the dev panel's "Show stats
/// overlay" checkbox — off by default. The FPS/frame-time smoothing lives in
/// [`StatsState`] (not `DevPanelState`) so updating it every frame doesn't
/// re-trigger the day/night lighting recompute.
pub fn render_stats_overlay(
    mut egui: ResMut<EguiManager>,
    dev: Res<crate::resources::DevPanelState>,
    time: Res<Time<Real>>,
    mut stats: ResMut<crate::resources::StatsState>,
    entities: Query<()>,
) {
    if !dev.show_stats {
        return;
    }
    // Exponential moving average so the readout is steady, not jittering per frame.
    const ALPHA: f32 = 0.1;
    let dt = time.delta_secs().max(1e-6);
    let inst_fps = 1.0 / dt;
    let inst_ms = dt * 1000.0;
    stats.fps = if stats.fps <= 0.0 {
        inst_fps
    } else {
        stats.fps + ALPHA * (inst_fps - stats.fps)
    };
    stats.frame_ms = if stats.frame_ms <= 0.0 {
        inst_ms
    } else {
        stats.frame_ms + ALPHA * (inst_ms - stats.frame_ms)
    };
    // `Query::count()`, not `iter().count()`: `QueryIter` doesn't override
    // `Iterator::count`, so the latter walks every entity in the world (tens of
    // thousands, with tiles + buildings streamed in) just to render one number.
    // `Query::count` takes the archetype `size_hint` fast path — same value.
    let entity_count = entities.count();

    let fps = stats.fps;
    let frame_ms = stats.frame_ms;
    // Left side is clear of the right-docked panels; offset down to clear the menu bar.
    egui::Area::new(egui::Id::new("stats_overlay"))
        .anchor(egui::Align2::LEFT_TOP, egui::vec2(10.0, 40.0))
        .show(egui.ctx_mut(), |ui| {
            theme::overlay_frame().show(ui, |ui| {
                let mono = |ui: &mut egui::Ui, text: String, color: egui::Color32| {
                    ui.label(
                        egui::RichText::new(text)
                            .monospace()
                            .size(12.5)
                            .color(color),
                    );
                };
                // Let the rows set the box's width instead of wrapping inside it.
                // A size-to-content `Area` settles at the width of its widest
                // *unwrapped* row, so the one long line ("27.80 ms/frame") folded
                // in two while the box stayed narrow — a readout that changes
                // line count as the number changes is unreadable at a glance.
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                ui.spacing_mut().item_spacing.y = 3.0;
                mono(ui, format!("{fps:5.1} fps"), theme::CREAM);
                mono(ui, format!("{frame_ms:5.2} ms/frame"), theme::TEXT_DIM);
                mono(ui, format!("{entity_count} entities"), theme::TEXT_DIM);
            });
        });
}

/// Fill the edit dialog's fields and revert snapshots from a prim. Leaves `visible` and
/// `active_tab` untouched, so it serves both opening the dialog and following a selection
/// change. Sets `is_new = false` and `prim_id` to this prim.
pub fn populate_edit_dialog(edit_dialog: &mut EditDialogState, prim: &Prim, transform: &Transform) {
    edit_dialog.prim_id = Some(prim.id);
    edit_dialog.is_new = false;
    edit_dialog.name = prim.name.clone();
    let color = {
        let c = prim.color.to_linear();
        [c.red, c.green, c.blue]
    };
    edit_dialog.color = color;
    let shape = format!("{:?}", prim.shape).to_lowercase();
    edit_dialog.shape = shape.clone();
    let pos = transform.translation;
    edit_dialog.position = [pos.x, pos.y, pos.z];
    let rot = transform.rotation.to_euler(EulerRot::XYZ);
    edit_dialog.rotation = [rot.0, rot.1, rot.2];
    let scale_v = transform.scale;
    edit_dialog.scale = [scale_v.x, scale_v.y, scale_v.z];
    edit_dialog.texture_id = prim.texture_id.clone();
    edit_dialog.path_cut_begin = prim.path_cut_begin;
    edit_dialog.path_cut_end = prim.path_cut_end;
    edit_dialog.hollow = prim.hollow;
    edit_dialog.surface = prim.surface;
    edit_dialog.repeats_per_meter = 0.0;
    edit_dialog.warp = crate::resources::PrimWarp {
        twist_begin: prim.twist_begin,
        twist_end: prim.twist_end,
        taper_x: prim.taper_x,
        taper_y: prim.taper_y,
        top_shear_x: prim.top_shear_x,
        top_shear_y: prim.top_shear_y,
        slice_begin: prim.slice_begin,
        slice_end: prim.slice_end,
    };
    // Snapshot for Cancel revert.
    edit_dialog.original_name = prim.name.clone();
    edit_dialog.original_color = color;
    edit_dialog.original_shape = shape;
    edit_dialog.original_position = edit_dialog.position;
    edit_dialog.original_rotation = edit_dialog.rotation;
    edit_dialog.original_scale = edit_dialog.scale;
    edit_dialog.original_texture_id = prim.texture_id.clone();
    edit_dialog.original_path_cut_begin = prim.path_cut_begin;
    edit_dialog.original_path_cut_end = prim.path_cut_end;
    edit_dialog.original_hollow = prim.hollow;
    edit_dialog.original_warp = edit_dialog.warp;
    edit_dialog.original_surface = prim.surface;
    edit_dialog.texture_picker_open = false;
}

/// While the edit dialog is open, keep it pointed at the currently selected prim so
/// clicking a different prim updates the dialog instead of leaving it on the old one.
pub fn sync_dialog_to_selection(
    mut edit_dialog: ResMut<EditDialogState>,
    mut game_state: ResMut<GameState>,
    prim_query: Query<(&Prim, &Transform)>,
) {
    // Only follow selection for an open editor of an existing prim (never clobber a
    // half-filled "Create Prim" dialog).
    if !edit_dialog.visible || edit_dialog.is_new {
        return;
    }
    let Some(selected) = game_state.selected_prim_id else {
        return;
    };
    if edit_dialog.prim_id == Some(selected) {
        return; // already showing the selected prim
    }
    for (prim, transform) in prim_query.iter() {
        if prim.id == selected {
            populate_edit_dialog(&mut edit_dialog, prim, transform);
            game_state.editing_prim_id = Some(selected);
            break;
        }
    }
}

/// Render context menu when a prim is right-clicked
pub fn render_context_menu(
    mut commands: Commands,
    mut egui: ResMut<EguiManager>,
    mut context_menu: ResMut<ContextMenuState>,
    mut edit_dialog: ResMut<EditDialogState>,
    mut game_state: ResMut<GameState>,
    prim_query: Query<(Entity, &Prim, &Transform)>,
    selected_query: Query<Entity, With<Selected>>,
    mut route: ResMut<crate::systems::navigation::RouteState>,
    mut right_panel: ResMut<crate::resources::RightPanel>,
) {
    use crate::resources::RightPanel;
    use crate::systems::navigation::Endpoint;

    if !context_menu.visible {
        return;
    }

    let ctx = egui.ctx_mut();
    let mut menu_open = true;

    // Position the context menu at the mouse click location
    let menu_pos = egui::pos2(context_menu.screen_pos.x, context_menu.screen_pos.y);

    Window::new("Context Menu")
        .open(&mut menu_open)
        .fixed_pos(menu_pos)
        .show(ctx, |ui| {
            // ── Routing, first and primary ──
            //
            // "Route to here" is the default act — a point on a map is a place you
            // want to *go*, and asking for the destination is the whole planner.
            // "Route from here" is the same gesture the other way round and is
            // deliberately the quieter of the two: needing it means you are
            // planning a journey that does not start where you are, which is the
            // rarer case.
            if let Some((lat, lng)) = context_menu.lat_lng {
                if ui
                    .button("🚩 Route to here")
                    .on_hover_text("Set this point as the destination")
                    .clicked()
                {
                    route.set_endpoint_at(Endpoint::To, lat, lng);
                    // The start fills itself in: opening the panel runs
                    // `RouteState::default_start`, which is where "here" is
                    // defined (the camera against the map anchor). Resolving it a
                    // second time here would mean carrying three more queries into
                    // a menu that has no other use for them.
                    *right_panel = RightPanel::Route;
                    context_menu.visible = false;
                }
                if ui
                    .small_button("Route from here")
                    .on_hover_text("Set this point as the starting point")
                    .clicked()
                {
                    route.set_endpoint_at(Endpoint::From, lat, lng);
                    *right_panel = RightPanel::Route;
                    context_menu.visible = false;
                }
                ui.separator();
            }

            if let Some(prim_id) = context_menu.hit_prim_id {
                // Clicked on an existing prim
                if ui.button("Edit Prim (E)").clicked() {
                    for entity in selected_query.iter() {
                        commands.entity(entity).remove::<Selected>();
                    }
                    for (entity, prim, transform) in prim_query.iter() {
                        if prim.id == prim_id {
                            commands.entity(entity).insert(Selected);
                            game_state.selected_prim_id = Some(prim_id);
                            game_state.editing_prim_id = Some(prim_id);
                            populate_edit_dialog(&mut edit_dialog, prim, transform);
                            edit_dialog.visible = true;
                            break;
                        }
                    }
                    context_menu.visible = false;
                }

                if ui.button("Delete Prim (D)").clicked() {
                    tracing::info!(prim_id, "deleting prim");
                    game_state.prims_to_delete.push(prim_id);
                    context_menu.visible = false;
                }
            } else if context_menu.hit_region_id.is_some() {
                // Clicked on empty space with a region to put a prim in. The menu
                // now also opens where there is no region at all (for the routing
                // entries above, and so `--offline` has a map right-click), so
                // this can no longer assume one exists.
                if ui.button("Create Prim (C)").clicked() {
                    edit_dialog.prim_id = None;
                    edit_dialog.is_new = true;
                    edit_dialog.name = "Prim".to_string();
                    edit_dialog.color = [0.5, 0.5, 0.5]; // neutral gray
                    edit_dialog.shape = "box".to_string();
                    edit_dialog.texture_id = None;
                    edit_dialog.texture_picker_open = false;
                    edit_dialog.path_cut_begin = 0.0;
                    edit_dialog.path_cut_end = 1.0;
                    edit_dialog.hollow = 0.0;
                    edit_dialog.warp = crate::resources::PrimWarp::default();
                    edit_dialog.surface = viberfox_core::PrimSurface::default();
                    edit_dialog.repeats_per_meter = 0.0;
                    let hit = context_menu.hit_point;
                    // Offset Y so the prim rests on the surface instead of being centred in it.
                    edit_dialog.position = [hit.x, hit.y + 0.5, hit.z];
                    edit_dialog.rotation = [0.0, 0.0, 0.0];
                    edit_dialog.scale = [1.0, 1.0, 1.0];
                    edit_dialog.visible = true;
                    game_state.editing_prim_id = None;
                    context_menu.visible = false;
                }
            }

            if ui.button("Cancel (ESC)").clicked() {
                context_menu.visible = false;
            }
        });

    if !menu_open {
        context_menu.visible = false;
    }
}

/// Render the prim edit dialog
pub fn render_edit_dialog(
    mut egui: ResMut<EguiManager>,
    mut edit_dialog: ResMut<EditDialogState>,
    mut game_state: ResMut<GameState>,
    texture_lib: Res<TextureLibrary>,
    mut gizmo_state: ResMut<crate::systems::gizmo::GizmoState>,
) {
    if !edit_dialog.visible {
        return;
    }

    let ctx = egui.ctx_mut();
    let mut dialog_open = true;

    let title = if edit_dialog.is_new {
        "Create Prim"
    } else {
        "Edit Prim"
    };

    Window::new(title)
        .open(&mut dialog_open)
        .default_width(320.0)
        .show(ctx, |ui| {
            // Tab bar
            ui.horizontal(|ui| {
                ui.selectable_value(&mut edit_dialog.active_tab, 0, "Edit");
                ui.selectable_value(&mut edit_dialog.active_tab, 1, "Object");
                ui.selectable_value(&mut edit_dialog.active_tab, 2, "Texture");
            });
            ui.separator();

            match edit_dialog.active_tab {
                // ── Edit tab ────────────────────────────────────────────────
                0 => {
                    use crate::systems::gizmo::GizmoMode;
                    egui::Grid::new("edit_grid")
                        .num_columns(2)
                        .spacing([8.0, 6.0])
                        .show(ui, |ui| {
                            ui.label("Name");
                            ui.text_edit_singleline(&mut edit_dialog.name);
                            ui.end_row();

                            ui.label("Tool");
                            ui.horizontal(|ui| {
                                ui.radio_value(
                                    &mut gizmo_state.mode,
                                    GizmoMode::Translate,
                                    "Move (T)",
                                );
                                ui.radio_value(
                                    &mut gizmo_state.mode,
                                    GizmoMode::Rotate,
                                    "Rotate (R)",
                                );
                                ui.radio_value(
                                    &mut gizmo_state.mode,
                                    GizmoMode::Scale,
                                    "Scale (S)",
                                );
                            });
                            ui.end_row();
                        });
                }
                // ── Object tab ──────────────────────────────────────────────
                1 => {
                    egui::Grid::new("object_grid")
                        .num_columns(2)
                        .spacing([8.0, 4.0])
                        .show(ui, |ui| {
                            ui.label("Building Block Type");
                            egui::ComboBox::from_id_salt("shape_combo")
                                .selected_text(shape_display_name(&edit_dialog.shape))
                                .show_ui(ui, |ui| {
                                    for (key, label) in SHAPES {
                                        ui.selectable_value(
                                            &mut edit_dialog.shape,
                                            key.to_string(),
                                            *label,
                                        );
                                    }
                                });
                            ui.end_row();
                        });

                    ui.separator();

                    // Position / Size / Rotation
                    egui::Grid::new("transform_grid")
                        .num_columns(4)
                        .spacing([4.0, 4.0])
                        .show(ui, |ui| {
                            ui.label("Position (meters)");
                            ui.add(
                                egui::DragValue::new(&mut edit_dialog.position[0])
                                    .prefix("X ")
                                    .speed(0.1),
                            );
                            ui.add(
                                egui::DragValue::new(&mut edit_dialog.position[1])
                                    .prefix("Y ")
                                    .speed(0.1),
                            );
                            ui.add(
                                egui::DragValue::new(&mut edit_dialog.position[2])
                                    .prefix("Z ")
                                    .speed(0.1),
                            );
                            ui.end_row();

                            ui.label("Size (meters)");
                            ui.add(
                                egui::DragValue::new(&mut edit_dialog.scale[0])
                                    .prefix("X ")
                                    .speed(0.1)
                                    .range(0.001..=f32::MAX),
                            );
                            ui.add(
                                egui::DragValue::new(&mut edit_dialog.scale[1])
                                    .prefix("Y ")
                                    .speed(0.1)
                                    .range(0.001..=f32::MAX),
                            );
                            ui.add(
                                egui::DragValue::new(&mut edit_dialog.scale[2])
                                    .prefix("Z ")
                                    .speed(0.1)
                                    .range(0.001..=f32::MAX),
                            );
                            ui.end_row();

                            ui.label("Rotation (degrees)");
                            let mut rot_x = edit_dialog.rotation[0].to_degrees();
                            if ui
                                .add(
                                    egui::DragValue::new(&mut rot_x)
                                        .prefix("X ")
                                        .speed(1.0)
                                        .suffix("°"),
                                )
                                .changed()
                            {
                                edit_dialog.rotation[0] = rot_x.to_radians();
                            }
                            let mut rot_y = edit_dialog.rotation[1].to_degrees();
                            if ui
                                .add(
                                    egui::DragValue::new(&mut rot_y)
                                        .prefix("Y ")
                                        .speed(1.0)
                                        .suffix("°"),
                                )
                                .changed()
                            {
                                edit_dialog.rotation[1] = rot_y.to_radians();
                            }
                            let mut rot_z = edit_dialog.rotation[2].to_degrees();
                            if ui
                                .add(
                                    egui::DragValue::new(&mut rot_z)
                                        .prefix("Z ")
                                        .speed(1.0)
                                        .suffix("°"),
                                )
                                .changed()
                            {
                                edit_dialog.rotation[2] = rot_z.to_radians();
                            }
                            ui.end_row();
                        });

                    ui.separator();

                    // Path Cut Begin and End
                    ui.label("Path Cut Begin and End");
                    ui.horizontal(|ui| {
                        let pce = edit_dialog.path_cut_end;
                        let pcb = edit_dialog.path_cut_begin;
                        ui.label("B");
                        ui.add(
                            egui::DragValue::new(&mut edit_dialog.path_cut_begin)
                                .speed(0.005)
                                .range(0.0..=pce)
                                .min_decimals(3)
                                .max_decimals(3),
                        );
                        ui.label("E");
                        ui.add(
                            egui::DragValue::new(&mut edit_dialog.path_cut_end)
                                .speed(0.005)
                                .range(pcb..=1.0)
                                .min_decimals(3)
                                .max_decimals(3),
                        );
                    });

                    ui.add_space(4.0);

                    // Hollow (stored 0–0.95, displayed 0–95)
                    ui.label("Hollow");
                    let mut hollow_pct = edit_dialog.hollow * 100.0;
                    if ui
                        .add(
                            egui::DragValue::new(&mut hollow_pct)
                                .speed(0.5)
                                .range(0.0..=95.0)
                                .min_decimals(1)
                                .max_decimals(1),
                        )
                        .changed()
                    {
                        edit_dialog.hollow = hollow_pct / 100.0;
                    }

                    ui.add_space(4.0);

                    // Twist Begin and End (degrees, −360..360)
                    ui.label("Twist Begin and End");
                    ui.horizontal(|ui| {
                        ui.label("B");
                        ui.add(
                            egui::DragValue::new(&mut edit_dialog.warp.twist_begin)
                                .speed(1.0)
                                .range(-360.0..=360.0)
                                .suffix("°"),
                        );
                        ui.label("E");
                        ui.add(
                            egui::DragValue::new(&mut edit_dialog.warp.twist_end)
                                .speed(1.0)
                                .range(-360.0..=360.0)
                                .suffix("°"),
                        );
                    });

                    ui.add_space(4.0);

                    // Taper (−1..1 per axis)
                    ui.label("Taper");
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::DragValue::new(&mut edit_dialog.warp.taper_x)
                                .prefix("X ")
                                .speed(0.01)
                                .range(-1.0..=1.0)
                                .max_decimals(2),
                        );
                        ui.add(
                            egui::DragValue::new(&mut edit_dialog.warp.taper_y)
                                .prefix("Y ")
                                .speed(0.01)
                                .range(-1.0..=1.0)
                                .max_decimals(2),
                        );
                    });

                    ui.add_space(4.0);

                    // Top Shear (−0.5..0.5 per axis)
                    ui.label("Top Shear");
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::DragValue::new(&mut edit_dialog.warp.top_shear_x)
                                .prefix("X ")
                                .speed(0.01)
                                .range(-0.5..=0.5)
                                .max_decimals(2),
                        );
                        ui.add(
                            egui::DragValue::new(&mut edit_dialog.warp.top_shear_y)
                                .prefix("Y ")
                                .speed(0.01)
                                .range(-0.5..=0.5)
                                .max_decimals(2),
                        );
                    });

                    ui.add_space(4.0);

                    // Slice Begin and End (trims the path 0..1, begin ≤ end)
                    ui.label("Slice Begin and End");
                    ui.horizontal(|ui| {
                        let sb = edit_dialog.warp.slice_begin;
                        let se = edit_dialog.warp.slice_end;
                        ui.label("B");
                        ui.add(
                            egui::DragValue::new(&mut edit_dialog.warp.slice_begin)
                                .speed(0.005)
                                .range(0.0..=se)
                                .min_decimals(3)
                                .max_decimals(3),
                        );
                        ui.label("E");
                        ui.add(
                            egui::DragValue::new(&mut edit_dialog.warp.slice_end)
                                .speed(0.005)
                                .range(sb..=1.0)
                                .min_decimals(3)
                                .max_decimals(3),
                        );
                    });
                }
                // ── Texture tab ─────────────────────────────────────────────
                _ => {
                    egui::Grid::new("texture_grid")
                        .num_columns(2)
                        .spacing([8.0, 6.0])
                        .show(ui, |ui| {
                            ui.label("Color");
                            let mut srgb = [
                                (edit_dialog.color[0] * 255.0) as u8,
                                (edit_dialog.color[1] * 255.0) as u8,
                                (edit_dialog.color[2] * 255.0) as u8,
                            ];
                            if ui.color_edit_button_srgb(&mut srgb).changed() {
                                edit_dialog.color = [
                                    srgb[0] as f32 / 255.0,
                                    srgb[1] as f32 / 255.0,
                                    srgb[2] as f32 / 255.0,
                                ];
                            }
                            ui.end_row();

                            ui.label("Texture");
                            ui.horizontal(|ui| {
                                ui.label(edit_dialog.texture_id.as_deref().unwrap_or("None"));
                                if !texture_lib.entries.is_empty() && ui.button("Pick…").clicked()
                                {
                                    edit_dialog.texture_picker_open =
                                        !edit_dialog.texture_picker_open;
                                }
                                if edit_dialog.texture_id.is_some() && ui.button("Clear").clicked()
                                {
                                    edit_dialog.texture_id = None;
                                }
                            });
                            ui.end_row();

                            // Transparency (SL "Transparency %": 0 = opaque, higher = more see-through).
                            ui.label("Transparency");
                            let mut transparency = (1.0 - edit_dialog.surface.alpha) * 100.0;
                            if ui
                                .add(
                                    egui::DragValue::new(&mut transparency)
                                        .speed(0.5)
                                        .range(0.0..=100.0)
                                        .suffix(" %")
                                        .max_decimals(1),
                                )
                                .changed()
                            {
                                edit_dialog.surface.alpha =
                                    (1.0 - transparency / 100.0).clamp(0.0, 1.0);
                            }
                            ui.end_row();

                            // Glow (0–1; emissive surface glow).
                            ui.label("Glow");
                            ui.add(
                                egui::DragValue::new(&mut edit_dialog.surface.glow)
                                    .speed(0.01)
                                    .range(0.0..=1.0)
                                    .max_decimals(2),
                            );
                            ui.end_row();

                            ui.label("Full Bright");
                            crate::systems::theme::checkbox(
                                ui,
                                &mut edit_dialog.surface.full_bright,
                                "",
                            );
                            ui.end_row();
                        });

                    ui.separator();

                    // Repeats Per Face (tiling count per axis, with optional flip).
                    ui.label("Repeats Per Face");
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::DragValue::new(&mut edit_dialog.surface.repeat_u)
                                .prefix("U ")
                                .speed(0.05)
                                .range(0.0..=100.0)
                                .max_decimals(3),
                        );
                        crate::systems::theme::checkbox(
                            ui,
                            &mut edit_dialog.surface.flip_u,
                            "Flip",
                        );
                        ui.add(
                            egui::DragValue::new(&mut edit_dialog.surface.repeat_v)
                                .prefix("V ")
                                .speed(0.05)
                                .range(0.0..=100.0)
                                .max_decimals(3),
                        );
                        crate::systems::theme::checkbox(
                            ui,
                            &mut edit_dialog.surface.flip_v,
                            "Flip",
                        );
                    });

                    ui.add_space(4.0);

                    // Texture rotation (degrees, −360..360).
                    ui.label("Rotation (degrees)");
                    ui.add(
                        egui::DragValue::new(&mut edit_dialog.surface.rotation)
                            .speed(1.0)
                            .range(-360.0..=360.0)
                            .suffix("°"),
                    );

                    ui.add_space(4.0);

                    // Repeats Per Meter: derives per-face repeats from object size on Apply.
                    ui.label("Repeats Per Meter");
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::DragValue::new(&mut edit_dialog.repeats_per_meter)
                                .speed(0.05)
                                .range(0.0..=10.0)
                                .max_decimals(3),
                        );
                        if ui.button("Apply").clicked() && edit_dialog.repeats_per_meter > 0.0 {
                            let rpm = edit_dialog.repeats_per_meter;
                            edit_dialog.surface.repeat_u = rpm * edit_dialog.scale[0];
                            edit_dialog.surface.repeat_v = rpm * edit_dialog.scale[1];
                        }
                    });

                    ui.add_space(4.0);

                    // Texture offset (fraction of a repeat).
                    ui.label("Offset");
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::DragValue::new(&mut edit_dialog.surface.offset_u)
                                .prefix("U ")
                                .speed(0.005)
                                .range(-1.0..=1.0)
                                .max_decimals(3),
                        );
                        ui.add(
                            egui::DragValue::new(&mut edit_dialog.surface.offset_v)
                                .prefix("V ")
                                .speed(0.005)
                                .range(-1.0..=1.0)
                                .max_decimals(3),
                        );
                    });
                }
            }

            ui.separator();

            // Dialog buttons — always visible regardless of active tab
            ui.horizontal(|ui| {
                if ui.button("Save").clicked() {
                    game_state.pending_prim_save = Some(edit_dialog.clone());
                    edit_dialog.visible = false;
                    game_state.editing_prim_id = None;
                }

                if ui.button("Cancel").clicked() {
                    push_revert(&mut game_state, &edit_dialog);
                    edit_dialog.visible = false;
                    game_state.editing_prim_id = None;
                }

                if !edit_dialog.is_new && ui.button("Delete").clicked() {
                    if let Some(id) = edit_dialog.prim_id {
                        game_state.prims_to_delete.push(id);
                    }
                    edit_dialog.visible = false;
                    game_state.editing_prim_id = None;
                }
            });
        });

    if !dialog_open {
        push_revert(&mut game_state, &edit_dialog);
        edit_dialog.visible = false;
        edit_dialog.texture_picker_open = false;
        game_state.editing_prim_id = None;
    }

    // Texture picker window
    if edit_dialog.texture_picker_open && edit_dialog.visible {
        let mut picker_open = edit_dialog.texture_picker_open;
        let mut clicked_texture: Option<String> = None;
        Window::new("Texture Library")
            .open(&mut picker_open)
            .default_width(380.0)
            .show(egui.ctx_mut(), |ui| {
                egui::ScrollArea::vertical()
                    .max_height(400.0)
                    .show(ui, |ui| {
                        let cols = 4usize;
                        egui::Grid::new("tex_grid")
                            .num_columns(cols)
                            .spacing([8.0, 8.0])
                            .show(ui, |ui| {
                                for (i, entry) in texture_lib.entries.iter().enumerate() {
                                    let selected =
                                        edit_dialog.texture_id.as_deref() == Some(&entry.id);
                                    let cell_clicked = ui
                                        .vertical(|ui| {
                                            let clicked = if let Some(handle) =
                                                texture_lib.egui_handles.get(&entry.id)
                                            {
                                                let sized = egui::load::SizedTexture::new(
                                                    handle.id(),
                                                    egui::vec2(72.0, 72.0),
                                                );
                                                let resp = ui.add(
                                                    egui::Image::new(sized)
                                                        .sense(egui::Sense::click()),
                                                );
                                                if selected {
                                                    ui.painter().rect_stroke(
                                                        resp.rect,
                                                        2.0,
                                                        egui::Stroke::new(
                                                            2.5_f32,
                                                            ui.visuals().selection.bg_fill,
                                                        ),
                                                        egui::StrokeKind::Outside,
                                                    );
                                                }
                                                resp.clicked()
                                            } else {
                                                // Fallback text tile while egui handle is pending.
                                                ui.add_sized(
                                                    [72.0, 72.0],
                                                    egui::Button::new(&entry.name)
                                                        .selected(selected),
                                                )
                                                .clicked()
                                            };
                                            ui.label(
                                                egui::RichText::new(&entry.name)
                                                    .small()
                                                    .line_height(Some(14.0)),
                                            );
                                            clicked
                                        })
                                        .inner;
                                    if cell_clicked {
                                        clicked_texture = Some(entry.id.clone());
                                    }
                                    if (i + 1) % cols == 0 {
                                        ui.end_row();
                                    }
                                }
                                if !texture_lib.entries.is_empty()
                                    && texture_lib.entries.len() % cols != 0
                                {
                                    ui.end_row();
                                }
                            });
                    });
            });
        if let Some(id) = clicked_texture {
            edit_dialog.texture_id = Some(id);
            picker_open = false;
        }
        edit_dialog.texture_picker_open = picker_open;
    }
}

/// Sync dialog values → prim entity every frame while the edit dialog is open.
/// This gives live preview; Save persists to DB, Cancel reverts via `pending_prim_revert`.
pub fn apply_live_prim_edits(
    mut commands: Commands,
    edit_dialog: Res<EditDialogState>,
    game_state: Res<GameState>,
    mut prim_query: Query<(
        Entity,
        &mut Prim,
        &mut Transform,
        Option<&MeshMaterial3d<StandardMaterial>>,
        Has<crate::components::OwnedMaterial>,
    )>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    if !edit_dialog.visible || edit_dialog.is_new {
        return;
    }
    let Some(prim_id) = game_state.editing_prim_id else {
        return;
    };
    for (entity, mut prim, mut transform, mat_handle, owned) in prim_query.iter_mut() {
        if prim.id != prim_id {
            continue;
        }
        transform.translation = Vec3::new(
            edit_dialog.position[0],
            edit_dialog.position[1],
            edit_dialog.position[2],
        );
        transform.rotation = Quat::from_euler(
            EulerRot::XYZ,
            edit_dialog.rotation[0],
            edit_dialog.rotation[1],
            edit_dialog.rotation[2],
        );
        transform.scale = Vec3::new(
            edit_dialog.scale[0],
            edit_dialog.scale[1],
            edit_dialog.scale[2],
        );
        let new_color = Color::srgb(
            edit_dialog.color[0],
            edit_dialog.color[1],
            edit_dialog.color[2],
        );
        prim.color = new_color;
        prim.name = edit_dialog.name.clone();

        // Detect geometry changes and request a mesh rebuild.
        let new_shape = PrimShape::from_str(&edit_dialog.shape);
        let geom_changed = new_shape != prim.shape
            || (edit_dialog.path_cut_begin - prim.path_cut_begin).abs() > 0.001
            || (edit_dialog.path_cut_end - prim.path_cut_end).abs() > 0.001
            || (edit_dialog.hollow - prim.hollow).abs() > 0.001
            || warp_differs(&edit_dialog.warp, &prim);

        if geom_changed {
            prim.shape = new_shape;
            prim.path_cut_begin = edit_dialog.path_cut_begin;
            prim.path_cut_end = edit_dialog.path_cut_end;
            prim.hollow = edit_dialog.hollow;
            apply_warp(&mut prim, &edit_dialog.warp);
            commands.entity(entity).insert(NeedsMeshRebuild);
        }

        // Update material directly — Selected doesn't change during editing so
        // highlight_selected_prim won't fire on its own. Copy-on-write out of the
        // shared pool so the live preview only restyles the prim being edited.
        if let Some(handle) = mat_handle {
            if let Some(mut mat) = crate::systems::rendering::owned_material(
                &mut commands,
                &mut materials,
                entity,
                &handle.0,
                owned,
            ) {
                // Live preview of the texture surface (transparency / glow / full-bright / UV),
                // then a subtle selection highlight on top so the surface stays visible.
                crate::systems::rendering::apply_surface(&mut mat, new_color, &edit_dialog.surface);
                crate::systems::rendering::apply_selection_highlight(&mut mat);
            }
        }
        break;
    }
}

fn push_revert(game_state: &mut GameState, dialog: &EditDialogState) {
    if !dialog.is_new {
        use crate::resources::EditDialogState;
        game_state.pending_prim_revert = Some(EditDialogState {
            prim_id: dialog.prim_id,
            name: dialog.original_name.clone(),
            shape: dialog.original_shape.clone(),
            position: dialog.original_position,
            rotation: dialog.original_rotation,
            scale: dialog.original_scale,
            color: dialog.original_color,
            original_texture_id: dialog.original_texture_id.clone(),
            path_cut_begin: dialog.original_path_cut_begin,
            path_cut_end: dialog.original_path_cut_end,
            hollow: dialog.original_hollow,
            warp: dialog.original_warp,
            surface: dialog.original_surface,
            ..Default::default()
        });
    }
}

/// True when any twist/taper/top-shear/slice value differs from the prim's.
fn warp_differs(w: &crate::resources::PrimWarp, prim: &Prim) -> bool {
    (w.twist_begin - prim.twist_begin).abs() > 0.001
        || (w.twist_end - prim.twist_end).abs() > 0.001
        || (w.taper_x - prim.taper_x).abs() > 0.001
        || (w.taper_y - prim.taper_y).abs() > 0.001
        || (w.top_shear_x - prim.top_shear_x).abs() > 0.001
        || (w.top_shear_y - prim.top_shear_y).abs() > 0.001
        || (w.slice_begin - prim.slice_begin).abs() > 0.001
        || (w.slice_end - prim.slice_end).abs() > 0.001
}

/// Copy the dialog's warp parameters onto a prim component.
fn apply_warp(prim: &mut Prim, w: &crate::resources::PrimWarp) {
    prim.twist_begin = w.twist_begin;
    prim.twist_end = w.twist_end;
    prim.taper_x = w.taper_x;
    prim.taper_y = w.taper_y;
    prim.top_shear_x = w.top_shear_x;
    prim.top_shear_y = w.top_shear_y;
    prim.slice_begin = w.slice_begin;
    prim.slice_end = w.slice_end;
}

/// Send prim authoring changes to the server (ADR-029: online-only, geo-anchored)
/// and apply them locally for immediate feedback.
pub fn send_prim_mutations(
    mut commands: Commands,
    mut game_state: ResMut<GameState>,
    mut prim_query: Query<(
        Entity,
        &mut Prim,
        &mut Transform,
        Option<&MeshMaterial3d<StandardMaterial>>,
        Has<crate::components::OwnedMaterial>,
    )>,
    region_query: Query<&Region>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    texture_cache: Res<PrimTextureCache>,
    online: Option<Res<OnlineSession>>,
    map_anchor: Res<crate::resources::MapAnchor>,
) {
    if let Some(dialog_state) = game_state.pending_prim_save.take() {
        if dialog_state.is_new {
            let region_id = region_query.iter().next().map(|r| r.id).unwrap_or(1);
            let pos = Vec3::new(
                dialog_state.position[0],
                dialog_state.position[1],
                dialog_state.position[2],
            );

            // ADR-029: prims are online-only and geo-anchored. Ask the server to
            // create the prim; its PrimUpsert echo spawns the entity locally with
            // the server-assigned id.
            let (Some(sess), Some(geo)) = (
                online.as_ref(),
                crate::systems::prim_geo::geo_from_world(pos, &map_anchor),
            ) else {
                tracing::warn!("cannot create prim: offline or world not anchored yet");
                return;
            };
            let _ = sess.intent_tx.unbounded_send(NetMessage::CreatePrim {
                request_id: 0,
                region_id,
                name: dialog_state.name.clone(),
                position: pos,
                rotation: Vec3::new(
                    dialog_state.rotation[0],
                    dialog_state.rotation[1],
                    dialog_state.rotation[2],
                ),
                scale: Vec3::new(
                    dialog_state.scale[0],
                    dialog_state.scale[1],
                    dialog_state.scale[2],
                ),
                color: dialog_state.color,
                texture_id: dialog_state.texture_id.clone(),
                shape: dialog_state.shape.clone(),
                surface: dialog_state.surface,
                geometry: dialog_state.geometry(),
                geo,
            });
            tracing::info!(name = %dialog_state.name, "sent CreatePrim (online)");
            return;
        } else if let Some(prim_id) = dialog_state.prim_id {
            // ADR-029: send the authoritative, geo-anchored update to the server.
            // We still apply it locally below for immediate feedback; the PrimUpsert
            // echo is skipped for the selected prim, so there's no fight.
            let pos = Vec3::new(
                dialog_state.position[0],
                dialog_state.position[1],
                dialog_state.position[2],
            );
            if let (Some(sess), Some(geo)) = (
                online.as_ref(),
                crate::systems::prim_geo::geo_from_world(pos, &map_anchor),
            ) {
                let _ = sess.intent_tx.unbounded_send(NetMessage::UpdatePrim {
                    request_id: 0,
                    prim_id,
                    position: pos,
                    rotation: Vec3::new(
                        dialog_state.rotation[0],
                        dialog_state.rotation[1],
                        dialog_state.rotation[2],
                    ),
                    scale: Vec3::new(
                        dialog_state.scale[0],
                        dialog_state.scale[1],
                        dialog_state.scale[2],
                    ),
                    color: dialog_state.color,
                    texture_id: dialog_state.texture_id.clone(),
                    name: dialog_state.name.clone(),
                    surface: dialog_state.surface,
                    geometry: dialog_state.geometry(),
                    geo,
                });
            }
            for (entity, mut prim, mut transform, mat_handle_opt, owned) in prim_query.iter_mut() {
                if prim.id == prim_id {
                    let texture_changed = prim.texture_id != dialog_state.texture_id;
                    let geom_changed = prim.shape != PrimShape::from_str(&dialog_state.shape)
                        || (prim.path_cut_begin - dialog_state.path_cut_begin).abs() > 0.0001
                        || (prim.path_cut_end - dialog_state.path_cut_end).abs() > 0.0001
                        || (prim.hollow - dialog_state.hollow).abs() > 0.0001
                        || warp_differs(&dialog_state.warp, &prim);

                    prim.name = dialog_state.name.clone();
                    prim.shape = PrimShape::from_str(&dialog_state.shape);
                    prim.color = Color::srgb(
                        dialog_state.color[0],
                        dialog_state.color[1],
                        dialog_state.color[2],
                    );
                    prim.texture_id = dialog_state.texture_id.clone();
                    prim.path_cut_begin = dialog_state.path_cut_begin;
                    prim.path_cut_end = dialog_state.path_cut_end;
                    prim.hollow = dialog_state.hollow;
                    prim.surface = dialog_state.surface;
                    apply_warp(&mut prim, &dialog_state.warp);
                    *transform = Transform::from_xyz(
                        dialog_state.position[0],
                        dialog_state.position[1],
                        dialog_state.position[2],
                    )
                    .with_rotation(Quat::from_euler(
                        EulerRot::XYZ,
                        dialog_state.rotation[0],
                        dialog_state.rotation[1],
                        dialog_state.rotation[2],
                    ))
                    .with_scale(Vec3::new(
                        dialog_state.scale[0],
                        dialog_state.scale[1],
                        dialog_state.scale[2],
                    ));
                    apply_material_edits(
                        &mut commands,
                        entity,
                        mat_handle_opt,
                        owned,
                        &mut materials,
                        &texture_cache,
                        dialog_state.texture_id.as_deref(),
                        texture_changed,
                        prim.color,
                        &prim.surface,
                    );
                    if geom_changed {
                        commands.entity(entity).insert(NeedsMeshRebuild);
                    }
                    commands.entity(entity).insert(Selected);
                    game_state.selected_prim_id = Some(prim_id);
                    tracing::info!(id = prim_id, "updated prim");
                    break;
                }
            }
        }
    }

    if let Some(revert) = game_state.pending_prim_revert.take() {
        if let Some(prim_id) = revert.prim_id {
            for (entity, mut prim, mut transform, mat_handle_opt, owned) in prim_query.iter_mut() {
                if prim.id == prim_id {
                    let texture_changed = prim.texture_id != revert.original_texture_id;
                    let geom_changed = prim.shape != PrimShape::from_str(&revert.shape)
                        || (prim.path_cut_begin - revert.path_cut_begin).abs() > 0.0001
                        || (prim.path_cut_end - revert.path_cut_end).abs() > 0.0001
                        || (prim.hollow - revert.hollow).abs() > 0.0001
                        || warp_differs(&revert.warp, &prim);

                    prim.name = revert.name.clone();
                    prim.shape = PrimShape::from_str(&revert.shape);
                    prim.color = Color::srgb(revert.color[0], revert.color[1], revert.color[2]);
                    prim.texture_id = revert.original_texture_id.clone();
                    prim.path_cut_begin = revert.path_cut_begin;
                    prim.path_cut_end = revert.path_cut_end;
                    prim.hollow = revert.hollow;
                    prim.surface = revert.surface;
                    apply_warp(&mut prim, &revert.warp);
                    *transform = Transform::from_xyz(
                        revert.position[0],
                        revert.position[1],
                        revert.position[2],
                    )
                    .with_rotation(Quat::from_euler(
                        EulerRot::XYZ,
                        revert.rotation[0],
                        revert.rotation[1],
                        revert.rotation[2],
                    ))
                    .with_scale(Vec3::new(
                        revert.scale[0],
                        revert.scale[1],
                        revert.scale[2],
                    ));
                    apply_material_edits(
                        &mut commands,
                        entity,
                        mat_handle_opt,
                        owned,
                        &mut materials,
                        &texture_cache,
                        revert.original_texture_id.as_deref(),
                        texture_changed,
                        prim.color,
                        &prim.surface,
                    );
                    if geom_changed {
                        commands.entity(entity).insert(NeedsMeshRebuild);
                    }
                    commands.entity(entity).insert(Selected);
                    game_state.selected_prim_id = Some(prim_id);
                    break;
                }
            }
        }
    }

    for prim_id in std::mem::take(&mut game_state.prims_to_delete) {
        if let Some(sess) = online.as_ref() {
            let _ = sess.intent_tx.unbounded_send(NetMessage::DeletePrim {
                request_id: 0,
                prim_id,
            });
        }
        for (entity, prim, _, _, _) in prim_query.iter() {
            if prim.id == prim_id {
                commands.entity(entity).despawn();
                tracing::info!(id = prim_id, "deleted prim");
                break;
            }
        }
    }
}

/// Applies a prim's texture (optionally) and surface params (transparency / glow /
/// full-bright / UV transform) onto its material in a single copy-on-write out of
/// the shared batching pool. Doing both in one call matters: the CoW clone is
/// taken from the pooled handle, so two separate CoWs in the same frame would each
/// start from the pool and the second would discard the first's change.
fn apply_material_edits(
    commands: &mut Commands,
    entity: Entity,
    mat_handle_opt: Option<&MeshMaterial3d<StandardMaterial>>,
    owned: bool,
    materials: &mut Assets<StandardMaterial>,
    cache: &PrimTextureCache,
    texture_id: Option<&str>,
    apply_texture: bool,
    color: Color,
    surface: &viberfox_core::PrimSurface,
) {
    let Some(mat_handle) = mat_handle_opt else {
        return;
    };
    let Some(mut mat) = crate::systems::rendering::owned_material(
        commands,
        materials,
        entity,
        &mat_handle.0,
        owned,
    ) else {
        return;
    };
    if apply_texture {
        mat.base_color_texture = texture_id
            .and_then(|id| cache.handles.get(id))
            .cloned()
            .map(|h| h.into());
    }
    crate::systems::rendering::apply_surface(&mut mat, color, surface);
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

const SHAPES: &[(&str, &str)] = &[
    ("box", "Box"),
    ("sphere", "Sphere"),
    ("cylinder", "Cylinder"),
    ("cone", "Cone"),
];

fn shape_display_name(key: &str) -> &str {
    SHAPES
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, label)| *label)
        .unwrap_or(key)
}
