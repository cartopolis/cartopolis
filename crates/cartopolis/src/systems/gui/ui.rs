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
use crate::systems::icons;
use crate::systems::theme;
use bevy::pbr::StandardMaterial;
use bevy::prelude::*;
use cartopolis_core::NetMessage;
use egui::Window;
use web_time::Instant;

/// The main menu's state: whether the card above the dock is showing.
///
/// A resource rather than a `Local` because two systems share it — the dock
/// paints the mark disc that toggles it (and shows it pressed while it is open),
/// and [`render_main_menu`] paints the card. `toggled` is that hand-off: without
/// it the click that opens the menu is also a click *outside* the card, which the
/// dismiss rule below would act on in the same frame, and the menu would never
/// appear.
#[derive(Resource, Default)]
pub struct MainMenu {
    pub open: bool,
    /// Set by the dock on the frame the disc was clicked; consumed here.
    pub toggled: bool,
}

/// Width of the menu card, in points. Fixed rather than sized to its content for
/// the reason the driver bar's module note gives: this is an anchored,
/// size-to-content `Area`, and anything inside it that reaches for the
/// *available* width would grow the card a little every frame.
const MENU_WIDTH: f32 = 232.0;

/// The app menu's account entry (`systems::session`), bundled into one parameter
/// because [`render_main_menu`] would otherwise sit at Bevy's 16-parameter
/// ceiling — the same reason `network::AuthWriteback` exists.
///
/// `session` is a `ResMut` because **Log out happens here**, not on the sign-in
/// card: it is one entry with one outcome, and routing it through a modal that
/// then has to be dismissed is three taps for a verb that needs one.
#[derive(bevy::ecs::system::SystemParam)]
pub struct SignInMenu<'w> {
    screen: ResMut<'w, crate::systems::login_screen::LoginScreen>,
    session: ResMut<'w, crate::systems::session::SessionState>,
    identity: Res<'w, crate::resources::AuthIdentity>,
}

/// The main menu: a card that rises out of the dock's mark disc, holding
/// everything that is not a place you navigate to.
///
/// **This is what is left of the address bar.** That was a card in the top-left
/// corner: a mark you clicked to unfold a search field, three history buttons, the
/// camera picker and a hamburger. It began as a full-width strip, then a card that
/// revealed itself when the pointer grazed the top edge (undiscoverable — you
/// cannot aim at something you have never seen — and fired on every trip to the
/// window's title bar), then the folding card. All three put the two controls
/// reached most often, search and the menu, in the corner furthest from the thumb
/// of anyone holding a phone, and all three had to fold half their contents into
/// the menu on a narrow viewport anyway. So the search field is a panel
/// (`geo_nav::render_search_panel`), the mark is a dock disc, and what the compact
/// layout folded away *permanently* lives here — one menu on every viewport,
/// rather than a menu whose contents depend on the window width.
///
/// Anchored to the bottom centre rather than to the disc it belongs to: the dock
/// is centred and its width depends on whether there is a session (Chat comes and
/// goes), so an offset measured from the disc would move under a card that should
/// not move. It is a floating `Area`, so like the dock and the HUD it is
/// deliberately *not* in the Phase 7b panel chain.
#[allow(clippy::too_many_arguments)]
pub fn render_main_menu(
    mut egui: ResMut<EguiManager>,
    mut menu: ResMut<MainMenu>,
    mut dev_state: ResMut<crate::resources::DevPanelState>,
    mut status: ResMut<crate::systems::hud::StatusHud>,
    mut store: ResMut<crate::systems::user_store::UserStore>,
    mut right_panel: ResMut<crate::resources::RightPanel>,
    mut app_exit: MessageWriter<AppExit>,
    mut sign_in: SignInMenu,
    // Written here, read and cleared by `diagnostics::share_diagnostics`.
    mut diagnostics: ResMut<crate::systems::diagnostics::DiagnosticsRequest>,
) {
    let dismissed_by_disc = std::mem::take(&mut menu.toggled);
    if !menu.open {
        return;
    }

    let bottom = crate::systems::dock::reserved_bottom() + 8.0;
    let max_height = (egui.viewport_pts().y - bottom - 24.0).max(120.0);
    let ctx = egui.ctx_mut().clone();
    let mut close = false;
    let mut log_out = false;

    let area = egui::Area::new(egui::Id::new("main_menu"))
        .order(egui::Order::Foreground)
        // **The sizing pass decides how tall this card may ever be.** An `Area`
        // lays out its first frame against `Spacing::default_area_size`, which
        // egui defaults to 600×**400**, then remembers the content's size and
        // uses *that* as the `max_rect` for every frame after. Overflow is
        // supposed to grow it back — "if the contents overflow, the area will
        // grow" — but only if the content reports its true height, and this one
        // cannot: the `ScrollArea` shrinks to the room it is given, and inside a
        // justified top-down layout the widgets past the bottom are allocated
        // whatever is left, which by then is a few points. The result is a
        // *stable* wrong answer — content and container agree at ~340 pt — so it
        // does not flicker or recover, it just ends.
        //
        // What that cost: the card stopped after About, and Sign in…, the status
        // readout, Developer tools and Exit were simply not on screen. Every one
        // of them is the only route to what it does. It reads as a menu that is
        // merely short, which is why it survived a UI sweep that was looking at
        // this very card.
        //
        // So the sizing pass gets the whole band the card is allowed, and the
        // remembered size is then the content's own.
        .default_size(egui::vec2(MENU_WIDTH, max_height))
        .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -bottom))
        .show(&ctx, |ui| {
            // `Frame::popup`, not `theme::overlay_frame`: the shared overlay frame
            // is translucent, which is right for a HUD readout and wrong under a
            // list of commands over a moving map.
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_width(MENU_WIDTH);
                egui::ScrollArea::vertical()
                    .max_height(max_height)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        ui.spacing_mut().button_padding.y = 4.0;
                        // The brand, at the top of the card the mark opened — the
                        // same two-tone wordmark the splash and the sign-in card
                        // wear (`theme::WORDMARK_HEAD`).
                        theme::wordmark_ui(ui, theme::SIZE_TITLE);
                        ui.separator();

                        // Justified, the way egui lays a menu out: every entry
                        // takes the card's full width, so the hit targets line up
                        // and a two-word label does not make a button half the
                        // size of its neighbour. The entries themselves are
                        // `theme::menu_entry` — a line of a list, not a plate;
                        // see that helper for what a card of full-width buttons
                        // looked like.
                        ui.with_layout(egui::Layout::top_down_justified(egui::Align::Min), |ui| {
                            // **The account is the first entry**, because it is the
                            // one thing in this card that changes what the app *is*
                            // rather than what it shows — and because signed out,
                            // it is the only thing here anybody is looking for.
                            //
                            // Signed in it is a single verb. It used to read
                            // "jeroen · cartopolis.org" and open the sign-in card
                            // again, where a Sign out button sat beside a Sign in
                            // one: a modal, to leave.
                            if sign_in.session.is_online() {
                                let who = match sign_in.identity.username.as_deref() {
                                    Some(user) => {
                                        format!(
                                            "Signed in as {user} · {}",
                                            sign_in.session.mode.where_()
                                        )
                                    }
                                    None => {
                                        format!("Signed in to {}", sign_in.session.mode.where_())
                                    }
                                };
                                if theme::menu_entry(ui, icons::LOGOUT, "Log out")
                                    .on_hover_text(who)
                                    .clicked()
                                {
                                    log_out = true;
                                    close = true;
                                }
                            } else if theme::menu_entry(ui, icons::KEY, "Sign in…")
                                .on_hover_text(
                                    "Share a world with other people on a simulator you can reach",
                                )
                                .clicked()
                            {
                                sign_in.screen.open = true;
                                close = true;
                            }
                            // **Why a session failure is reported here.** The
                            // sign-in card used to raise itself whenever one
                            // arrived — a modal the app pushed in front of you,
                            // mid-flight, for a question you had not asked. It
                            // opens from the entry above and from nowhere else
                            // now, so the reason a remembered session did not
                            // come up, or a live one dropped, lands next to the
                            // entry that offers to try again. Losing it entirely
                            // would leave a client that silently fell back to
                            // the offline map with nothing anywhere saying why.
                            if let Some(error) = &sign_in.session.error {
                                theme::note(ui, error);
                            }
                            ui.separator();

                            // Panels you *visit* rather than flip: Appearance is a
                            // wardrobe and the Assistant is a conversation, so neither
                            // earns a permanent disc in the dock next to the places you
                            // actually navigate between.
                            for panel in [
                                crate::resources::RightPanel::Appearance,
                                crate::resources::RightPanel::Assistant,
                                crate::resources::RightPanel::Settings,
                                // About carries the attribution the map data's licences
                                // require, so it must be reachable from the running app,
                                // not only from a file in the tarball.
                                crate::resources::RightPanel::About,
                            ] {
                                // Same glyph the panel's own header wears, so the menu
                                // entry and the badge it opens are recognisably one thing.
                                if theme::menu_entry_selected(
                                    ui,
                                    right_panel.is(panel),
                                    panel.icon(),
                                    panel.label(),
                                )
                                .clicked()
                                {
                                    right_panel.toggle(panel);
                                    close = true;
                                }
                            }
                            ui.separator();
                            // The two things that switch an *instrument* on over
                            // the map, grouped because that is what they have in
                            // common. The status readout used to be a small ⓘ disc
                            // at the foot of the compass column, where being a
                            // different size was the only thing saying it was a
                            // different kind of control — see `hud`'s module note.
                            //
                            // They carry the accent when they are on rather than
                            // renaming themselves to "Hide …": a toggle that is
                            // lit says the same thing in a word less, and it is
                            // the state the panel entries above already show.
                            if theme::menu_entry_selected(
                                ui,
                                status.open,
                                icons::ABOUT,
                                "Status readout",
                            )
                            .on_hover_text(
                                "Time, position, altitude, and the sun and weather \
                                 switchers, over the top-right of the map",
                            )
                            .clicked()
                            {
                                status.open = !status.open;
                                close = true;
                            }
                            if theme::menu_entry_selected(
                                ui,
                                dev_state.open,
                                icons::TUNE,
                                "Developer tools",
                            )
                            .clicked()
                            {
                                dev_state.open = !dev_state.open;
                                close = true;
                            }
                            // Next to the developer tools because it is one — but
                            // above the fold rather than inside the dev panel,
                            // because the person who needs it is holding a phone
                            // and reporting a bug, not inspecting the renderer.
                            // It is the only way anything a device knows about
                            // itself gets off that device short of a crash
                            // (`systems::diagnostics`).
                            if theme::menu_entry(ui, icons::COPY, "Share diagnostics…")
                                .on_hover_text(
                                    "The build, the GPU, the render tier and what the \
                                     avatar is doing, plus this run's log — to the share \
                                     sheet on a phone, to the clipboard everywhere else",
                                )
                                .clicked()
                            {
                                diagnostics.pending = true;
                            }
                            if let Some(result) = diagnostics.last.as_deref() {
                                theme::note(ui, result);
                            }
                            ui.separator();
                            if theme::menu_entry(ui, icons::CLEAR, "Exit").clicked() {
                                app_exit.write(AppExit::Success);
                                close = true;
                            }
                        });
                    });
            });
        });

    if log_out {
        crate::systems::session::sign_out(&mut sign_in.session, &mut store);
    }
    // Dismiss on a click that landed outside the card — but never on the one that
    // opened it, which is a click outside the card by definition (see
    // `MainMenu::toggled`). Escape closes it too, the way every other transient
    // surface in this app does.
    let clicked_out = ctx.input(|i| {
        i.pointer.any_click()
            && i.pointer
                .interact_pos()
                .is_some_and(|p| !area.response.rect.contains(p))
    });
    // Escape is not read here. It closed nothing from this site anyway — the key
    // reached egui only while a text field had focus, and this card contains no
    // field — and now that it does reach egui, one handler per surface would
    // close two things per press. `systems::dismiss` owns the whole ladder.
    if close || (clicked_out && !dismissed_by_disc) {
        menu.open = false;
    }
}

// The camera-mode picker (a Follow/Fly/Earth chip row plus a "View whole
// Earth" jump) used to be drawn inline here. It is the mode disc in the
// bottom-right corner of the map now (`map_osd::mode_button`): a camera switch
// is a change to the map and belongs on it, not two taps deep in a card that
// covers it — and the disc, cycling only the modes on offer, is also where
// "Follow needs a signed-in session" is enforced for the UI.

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

/// How far right the frame overlay's box reaches, near enough.
///
/// The overlay is size-to-content — deliberately, see [`render_stats_overlay`] —
/// so this is a bound rather than a measurement, and it is used for exactly one
/// thing: keeping the developer panel's *default* position off it. A window the
/// user can drag does not need an exact answer, and paying for one (laying the
/// overlay out first, then reading its rect back) would couple two systems that
/// currently share nothing.
const STATS_OVERLAY_RIGHT: f32 = 160.0;

/// How often the draw/triangle census is re-walked while the panel is open.
///
/// [`RenderCensus::counts`] resolves every mesh handle in the world, which is a
/// real hitch on a streamed city — the module note is explicit that it is cheap
/// enough once per capture or once per five seconds and not cheap enough per
/// frame. Twice a second is the compromise a live panel needs: slow enough not to
/// be the thing it is measuring, fast enough that a number moves when the scene
/// does. The frame timing beside it is *not* throttled by this — that comes from
/// [`crate::resources::FrameStats`], which is accumulated every frame regardless.
const CENSUS_PERIOD_S: f64 = 0.5;

/// The throttled census, kept across frames so the panel is not re-walking the
/// world on every one of them.
#[derive(Default)]
pub struct CensusCache {
    counts: crate::systems::render_census::RenderCounts,
    /// `Time<Real>` seconds at the last walk. `None` until the first.
    at: Option<f64>,
}

/// Developer tools (Ctrl+Shift+D, or the menu): what this run is *doing*, as
/// distinct from what it is set to.
///
/// **The clock and the weather are not here any more** — they went to Settings ▸
/// World, where a phone can reach them, and the panel got what actually needs a
/// developer in exchange. The distinction the two panels now draw is worth
/// stating, because it is what decides where the next control goes: Settings is
/// where you change the world or what the machine spends on it, and this is where
/// you find out what happened. Nothing in here is a setting except the two
/// checkboxes at the bottom, and both of those switch a *readout* on.
///
/// Everything it shows already existed and was being thrown away. `FrameStats` is
/// accumulated every frame for the diagnostics report and was legible nowhere;
/// `RenderCensus` was computed on a five-second log timer that is switched off on
/// handsets; `UploadMeter` reached a `slow frame` warning and `--dump-state`;
/// `pipeline_health` reached the shareable report alone. On a workstation all four
/// live in a terminal — which is a fine place for them right up until the question
/// is "is it doing that *now*, while I hold this key down", and then it is not a
/// place at all. So this panel invents no instrument: it is one window onto four
/// that were already measuring.
///
/// **Frame rate came back here first.** It moved out with the render settings in
/// the Graphics-tab migration, on the correct argument that a phone cannot press
/// Ctrl+Shift+D — but the overlay it became is off by default and three taps deep
/// in Settings, so on a desktop the number simply vanished. Both are right: the
/// overlay is the handset's frame counter and stays where it is, and the panel a
/// developer already has open shows it without being asked.
#[allow(clippy::too_many_arguments)]
pub fn render_dev_panel(
    mut egui: ResMut<EguiManager>,
    mut dev: ResMut<crate::resources::DevPanelState>,
    mut shot: ResMut<crate::resources::ScreenshotState>,
    mut prefs: ResMut<crate::resources::AppPrefs>,
    // Accumulated every frame by `lib.rs::frame_stats`, whether anything is
    // reading it or not — so it is honest the instant the panel opens, with no
    // warm-up and no "0 fps" first second.
    frame: Res<crate::resources::FrameStats>,
    upload: Res<crate::systems::upload_meter::UploadMeter>,
    census: crate::systems::render_census::RenderCensus,
    entities: Query<()>,
    time: Res<Time<Real>>,
    // For "Copy as preset" and the position readout: the same camera fix the HUD
    // reads.
    globe: Res<crate::systems::globe::GlobeState>,
    camera: Query<(&big_space::prelude::GridCell, &Transform), With<crate::components::FreeCamera>>,
    map_anchor: Res<crate::resources::MapAnchor>,
    mut copied_at: Local<Option<Instant>>,
    mut cache: Local<CensusCache>,
) {
    if !dev.open {
        return;
    }
    // Every number in this panel changes on its own, so the window has to be
    // redrawn on its own — without this the readout only updates on the frames
    // something else caused, i.e. it measures the frames *you* caused, which is
    // the opposite of what it is for. Same call and same reason as the overlay.
    egui.animating();

    let now = time.elapsed_secs_f64();
    if cache.at.is_none_or(|t| now - t >= CENSUS_PERIOD_S) {
        cache.counts = census.counts();
        cache.at = Some(now);
    }
    let counts = cache.counts;
    let entity_count = entities.count();
    let fix = camera
        .single()
        .ok()
        .zip(map_anchor.geo)
        .map(|((cell, tf), anchor)| {
            let (lat, lng) = crate::systems::geo_nav::camera_geo(anchor, &globe, cell, tf);
            // (yaw, pitch, roll) — the same decomposition free_camera uses to seed
            // its look angles from the live view, so a pasted preset reproduces
            // this exact framing.
            let euler = tf.rotation.to_euler(EulerRot::YXZ);
            ViewFix {
                lat,
                lng,
                alt: globe.altitude_m(cell, tf),
                pitch: euler.1.to_degrees(),
                yaw: euler.0.to_degrees(),
                in_space: globe.in_space(cell, tf),
                space_factor: globe.space_factor(cell, tf),
            }
        });
    // Edited as a plain bool and written back below, so the panel does not mark
    // `AppPrefs` changed on every frame it is open — the same shape the Settings
    // panel uses, and the reason its module note gives.
    let mut show_stats = prefs.show_stats;

    let ctx = egui.ctx_mut();
    let mut open = true;
    egui::Window::new("Developer tools")
        .open(&mut open)
        .default_width(290.0)
        // Clear of the top-left corner, which by now holds the disc column *and*
        // the frame overlay under it. egui's default cascade puts a window near
        // that corner, so the panel opened on top of the very readout its own
        // checkbox switches on. Only a default — the window is draggable and egui
        // remembers where it was put.
        .default_pos([STATS_OVERLAY_RIGHT + theme::OSD_GAP, theme::OSD_MARGIN])
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing.y = 3.0;

            // ── Frame ──────────────────────────────────────────────────────
            // Mean, worst and jitter together, because they fail differently: a
            // healthy mean beside a bad worst is a hitch, and a healthy mean
            // beside a high jitter is a pacing problem rather than a load one.
            // A single fps number hides both.
            theme::section(ui, "Frame");
            if frame.sampled {
                stat(ui, "rate", &format!("{} fps", frame.fps));
                stat(
                    ui,
                    "frame",
                    &format!(
                        "{:.1} ms  worst {:.0}  ±{:.1}",
                        frame.mean_ms, frame.worst_ms, frame.jitter_ms
                    ),
                );
            } else {
                // The window is a whole second wide, so say so rather than print
                // a confident zero — the same call `FrameStats` documents.
                stat(ui, "rate", "measuring…");
            }

            ui.add_space(5.0);
            theme::section(ui, "Drawn");
            stat(
                ui,
                "draws",
                &format!("{}  ·  {} entities", counts.draws, entity_count),
            );
            stat(ui, "triangles", &thousands(counts.tris_total));
            // Split by producer, so a regression names the streamer that caused
            // it. Shadow casters are counted apart because each cascade re-draws
            // them — four cascades make that number the one that matters.
            stat(
                ui,
                "  shadowed",
                &format!(
                    "{} ({:.0}%)",
                    thousands(counts.tris_shadow),
                    share(counts.tris_shadow, counts.tris_total),
                ),
            );
            stat(ui, "  buildings", &thousands(counts.tris_buildings));
            stat(ui, "  3DBAG", &thousands(counts.tris_lod22));
            stat(ui, "  map tiles", &thousands(counts.tris_tiles));
            stat(ui, "  globe", &thousands(counts.tris_globe));

            ui.add_space(5.0);
            // ── GPU ────────────────────────────────────────────────────────
            // The two halves of every device-loss report this project has:
            // a rate (a frame's upload burst times out a command buffer) and a
            // total (streamed content exhausts an allocator). They move
            // independently — the per-frame ceiling flattens the first and does
            // nothing at all about the second — so both are here or neither is
            // worth having. `docs/notes/upload-bursts.md`.
            theme::section(ui, "GPU");
            stat(
                ui,
                "upload",
                &format!(
                    "{} kB now · {} kB peak",
                    thousands(upload.frame_kb()),
                    thousands(upload.peak_kb()),
                ),
            );
            stat(
                ui,
                "biggest asset",
                &format!("{} kB", thousands(upload.peak_asset_kb())),
            );
            let resident_mb = upload.resident_bytes as f64 / (1024.0 * 1024.0);
            let peak_mb = upload.peak_resident_bytes as f64 / (1024.0 * 1024.0);
            stat(
                ui,
                "resident",
                &format!("{resident_mb:.0} MB · {peak_mb:.0} MB peak"),
            );
            // A pipeline that fails to compile is a silent hole in the picture:
            // the render node just skips its pass, and wgpu's one log line about
            // it has long since rolled out of the ring. Named here, live.
            match crate::systems::pipeline_health::snapshot() {
                Some(health) if !health.errored.is_empty() => {
                    stat_colored(
                        ui,
                        "pipelines",
                        &format!("{} FAILED", health.errored.len()),
                        theme::ERROR,
                    );
                    for label in &health.errored {
                        ui.label(
                            egui::RichText::new(label)
                                .monospace()
                                .size(theme::SIZE_TINY)
                                .color(theme::ERROR),
                        );
                    }
                }
                Some(health) => stat(
                    ui,
                    "pipelines",
                    &format!("{} ready · {} building", health.ready, health.building),
                ),
                None => stat(ui, "pipelines", "not measured"),
            }

            ui.add_space(5.0);
            // ── View ───────────────────────────────────────────────────────
            // Where the camera is, in the terms the rest of the client argues
            // in: a geographic fix, the altitude that drives every tier
            // decision, and `space_factor` — the 0..1 crossfade the whole space
            // tier rides, which is invisible in a screenshot and is the first
            // thing to check when a ramp looks like a cut.
            theme::section(ui, "View");
            match fix {
                Some(v) => {
                    stat(ui, "at", &format!("{:.5}, {:.5}", v.lat, v.lng));
                    stat(ui, "altitude", &metres(v.alt));
                    stat(
                        ui,
                        "look",
                        &format!("{:.1}° pitch · {:.1}° yaw", v.pitch, v.yaw),
                    );
                    stat(
                        ui,
                        "tier",
                        &if v.in_space {
                            format!("space · factor {:.2}", v.space_factor)
                        } else {
                            "ground".to_string()
                        },
                    );
                }
                None => stat(ui, "at", "no camera / no anchor yet"),
            }

            ui.add_space(5.0);
            theme::section(ui, "Readouts");
            // The overlay's own switch is in Settings ▸ Graphics, where a phone
            // can reach it. Mirrored here because this is the panel somebody
            // debugging a frame rate already has open, and making them close it
            // to find a checkbox is the kind of friction that ends with the
            // checkbox never being found.
            theme::checkbox(ui, &mut show_stats, "Frame overlay, over the map");
            // No "▸" here, or anywhere else a user reads. It is not in the
            // bundled brand font, so it paints a tofu box — the same trap
            // `theme::close_button` exists for. Doc comments are free to use it;
            // strings that reach epaint are not.
            theme::note(
                ui,
                "The clock and the weather are in Settings, under World.",
            );

            ui.add_space(5.0);
            theme::section(ui, "README screenshots");
            // The documentation set is generated from the remembered camera
            // positions in `docs/shots.toml` (`cargo shots`), not from this
            // button — but a *new* position has to be found by flying to it, so
            // this copies the current camera out as a pasteable `[[shot]]` block.
            // That is the whole handoff from "looks good here" to reproducible.
            ui.horizontal(|ui| {
                let copied = copied_at.is_some_and(|t| t.elapsed().as_secs_f32() < 1.5);
                if ui
                    .add_enabled(
                        fix.is_some(),
                        egui::Button::new(format!("{}  Copy as preset", icons::COPY)),
                    )
                    .on_hover_text("Copies this camera as a [[shot]] block for docs/shots.toml")
                    .clicked()
                {
                    // The same fix the View section above is reading. It used to
                    // be measured a second time here, from the same query, on
                    // every frame the panel was open — for a value only a click
                    // consumes.
                    if let Some(v) = fix {
                        crate::utils::clipboard::set_text(
                            &crate::systems::shot_presets::preset_toml(
                                "new-view",
                                v.lat,
                                v.lng,
                                v.alt,
                                v.pitch,
                                v.yaw,
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
                    .button(format!(
                        "{}  Capture slot {}",
                        icons::CAMERA,
                        shot.next_slot + 1
                    ))
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
    if show_stats != prefs.show_stats {
        prefs.show_stats = show_stats;
    }
    // Closing via the window's [x] only writes when it actually changed, to avoid
    // marking the resource changed every frame (which would re-run the lighting update).
    if !open && dev.open {
        dev.open = false;
    }
}

/// A camera fix, measured once per frame and read by both the View section and
/// the "Copy as preset" button.
#[derive(Clone, Copy)]
struct ViewFix {
    lat: f64,
    lng: f64,
    alt: f64,
    pitch: f32,
    yaw: f32,
    in_space: bool,
    /// The 0..1 crossfade the whole space tier rides. Worth a line of its own
    /// because it is invisible in a screenshot and is the first thing to check
    /// when a ramp — exposure, bloom, atmosphere density, globe albedo — reads
    /// as a hard cut.
    space_factor: f32,
}

/// One `name  value` row of a readout, with the name in a fixed column so a
/// stack of them lines up and the value monospaced so a changing digit does not
/// shift the ones beside it.
///
/// This is the whole layout vocabulary of the developer panel, deliberately: a
/// panel of numbers wants to be scanned down the left edge, and the moment two
/// rows disagree about where the value starts it stops being scannable.
fn stat(ui: &mut egui::Ui, name: &str, value: &str) {
    stat_colored(ui, name, value, theme::CREAM);
}

/// [`stat`] with the value in a chosen colour — for the one row that reports a
/// failure rather than a measurement.
fn stat_colored(ui: &mut egui::Ui, name: &str, value: &str, color: egui::Color32) {
    ui.horizontal(|ui| {
        ui.add_sized(
            [88.0, ui.spacing().interact_size.y],
            egui::Label::new(
                egui::RichText::new(name)
                    .size(theme::SIZE_SUB)
                    .color(theme::TEXT_DIM),
            ),
        );
        ui.label(
            egui::RichText::new(value)
                .monospace()
                .size(theme::SIZE_MONO)
                .color(color),
        );
    });
}

/// `1234567` → `"1 234 567"`. A thin space rather than a comma because these are
/// counts read at a glance, and six unbroken digits are the reason a triangle
/// budget is hard to eyeball.
fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push('\u{202f}');
        }
        out.push(c);
    }
    out
}

/// `part` as a percentage of `whole`, and 0 rather than a NaN on an empty scene.
fn share(part: usize, whole: usize) -> f32 {
    if whole == 0 {
        0.0
    } else {
        part as f32 * 100.0 / whole as f32
    }
}

/// An altitude in the unit that suits it: metres near the ground, kilometres
/// once the number stops fitting in a glance.
fn metres(m: f64) -> String {
    if m.abs() >= 10_000.0 {
        format!("{:.0} km", m / 1000.0)
    } else {
        format!("{m:.0} m")
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
    // Seed the draft from the saved appearance the first time the panel opens.
    if right_panel.is(RightPanel::Appearance) && !chooser.seeded {
        chooser.draft = store.data.appearance.clone();
        chooser.seeded = true;
    }
    // Borrowed, not cloned: this ran five String clones per frame whether the
    // panel was open or not, and the closure only compares against it — the
    // store write on Apply happens after the closure ends.
    let saved = &store.data.appearance;
    let mut apply = false;
    let closed = crate::systems::egui_manager::right_panel(
        &mut egui,
        &mut right_panel,
        RightPanel::Appearance,
        "appearance_panel",
        280.0,
        |ui| {
            // Outside the scroll area, so the header and the close button stay put
            // while the body scrolls under them — and so the body is what is
            // reachable on a 609 pt viewport, where these rows overrun the screen.
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
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
                    ui.label("Customise");
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
                    let changed = *d != *saved;
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
        },
    );
    if apply {
        store.data.appearance = chooser.draft.clone();
        store.dirty = true;
    }
    if closed {
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
/// frame time, and live entity count. Switched on from Settings ▸ Graphics — off
/// by default. The FPS/frame-time smoothing lives in [`StatsState`] (not in the
/// preference) so updating it every frame doesn't re-trigger the day/night
/// lighting recompute.
pub fn render_stats_overlay(
    mut egui: ResMut<EguiManager>,
    prefs: Res<crate::resources::AppPrefs>,
    time: Res<Time<Real>>,
    mut stats: ResMut<crate::resources::StatsState>,
    // The worst frame of the last whole second. The EMA below cannot show it —
    // smoothing is what a hitch disappears into — and a hitch is what somebody
    // switches this overlay on to see. Free: `lib.rs::frame_stats` accumulates it
    // every frame whether anything reads it or not.
    frame: Res<crate::resources::FrameStats>,
    entities: Query<()>,
) {
    if !prefs.show_stats {
        return;
    }
    // A frame-rate readout that only updates when you move the mouse measures the
    // frames you caused, which is the opposite of what it is for. See
    // `EguiManager::animating`.
    egui.animating();
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
    // Left side, because the right is where the panels dock — and *below the disc
    // column*, which is the part that was wrong. The old offset was a bare `40.0`
    // written against a menu bar that no longer exists; the layers disc, the
    // compass and the locate button have owned that corner since, and they are
    // floating `Area`s drawn over this one. A frame counter behind two discs is
    // not a frame counter. `hud::left_column_bottom` is the shared figure, so the
    // next control to grow on that column moves this one with it.
    egui::Area::new(egui::Id::new("stats_overlay"))
        .anchor(
            egui::Align2::LEFT_TOP,
            egui::vec2(
                theme::OSD_MARGIN,
                crate::systems::hud::left_column_bottom() + theme::OSD_GROUP_GAP,
            ),
        )
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
                if frame.sampled {
                    mono(
                        ui,
                        format!("{:5.0} ms worst", frame.worst_ms),
                        // Amber once a frame in the last second took longer than
                        // two 60 Hz frames. A hitch that has to be noticed in a
                        // number nobody is looking at is a hitch nobody notices.
                        if frame.worst_ms > 33.0 {
                            theme::AMBER
                        } else {
                            theme::TEXT_DIM
                        },
                    );
                }
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
    // half-filled "Create object" dialog).
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

/// The map's context menu: right-click on a desktop, **long-press on a phone**
/// (`systems::touch` fires an emulated `Right` for a held finger, which is what
/// `picking` opens this on).
///
/// It is reached from a point on the map rather than from a panel, so it is where
/// the acts that take a *coordinate* live: route to or from here, and save here as
/// a place.
///
/// At 15 system parameters this is one short of Bevy's cap of 16. The next thing
/// that needs world access here should be grouped into a `SystemParam` rather than
/// added to the list — `camera_controls` hit the same wall and that is what it
/// did.
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
    mut script_editor: ResMut<crate::systems::script_editor::ScriptEditorState>,
    online: Option<Res<crate::resources::OnlineSession>>,
    // Saving the point under the cursor as a place. `BookmarksState` is `Res` and
    // not `ResMut` on purpose: `request_address` resolves the name through an
    // interior-mutability channel, so this menu does not have to contend with the
    // Places panel for the resource every frame it happens to be open.
    mut store: ResMut<crate::systems::user_store::UserStore>,
    bookmarks: Res<crate::systems::bookmarks::BookmarksState>,
    camera_state: Res<crate::resources::CameraState>,
    camera: Query<(&big_space::prelude::GridCell, &Transform), With<crate::components::FreeCamera>>,
) {
    use crate::resources::RightPanel;
    use crate::systems::navigation::Endpoint;

    if !context_menu.visible {
        return;
    }

    // Read before the menu closure, which already borrows half the world: this is
    // one number, and taking the query in there as well buys nothing.
    let camera_altitude = camera.single().ok().map(|(cell, tf)| {
        cell.y as f64 * crate::grid::WORLD_CELL_EDGE as f64 + tf.translation.y as f64
    });

    // Position the context menu where the click landed. `screen_pos` came from
    // `Window::cursor_position` (picking), so it is in window points and needs
    // converting before egui can be asked to put anything there.
    let menu_pos = egui.window_to_ui(context_menu.screen_pos);

    let ctx = egui.ctx_mut();

    // No title bar: this is a context menu, and egui's stock window chrome gave it
    // a draggable caption reading "Context Menu" plus a collapse triangle — three
    // affordances a menu does not have. The last entry is the way out (there is no
    // Escape handler, and a phone has no Escape key), so nothing is lost with the
    // caption's ✕.
    Window::new("context menu")
        .id(egui::Id::new("prim_context_menu"))
        .title_bar(false)
        .resizable(false)
        .frame(egui::Frame::popup(&ctx.style_of(ctx.theme())))
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
                    .button(format!("{}  Route to here", icons::FLAG))
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
                if theme::action_button(ui, "Route from here")
                    .on_hover_text("Set this point as the starting point")
                    .clicked()
                {
                    route.set_endpoint_at(Endpoint::From, lat, lng);
                    *right_panel = RightPanel::Route;
                    context_menu.visible = false;
                }

                // ── Save the point you pointed at ──
                //
                // The same save-place button the corner overlay wears, because it is the same act
                // with a different subject: the overlay's saves *where the camera
                // is*, this saves *where you tapped*. Until now the second one had
                // to be done by flying there first and then pressing save, which on a
                // phone means losing the view you were looking at to record a spot
                // inside it.
                //
                // Altitude and pose come from the camera rather than from the hit
                // point, because a saved place is a remembered *view* as well as a
                // coordinate (`CameraPose::apply`) — a place saved from orbit that
                // returned you to ground level would be a different place.
                if let Some(altitude) = camera_altitude {
                    let star = icons::BOOKMARK_ADD;
                    if ui
                        .button(format!("{star}  Save this place"))
                        .on_hover_text("Add this point to Places, named by reverse lookup")
                        .clicked()
                    {
                        let id = store.add_bookmark(
                            crate::systems::bookmarks::PLACEHOLDER_NAME.to_string(),
                            lat,
                            lng,
                            altitude,
                            crate::systems::geo_nav::CameraPose::capture(&camera_state),
                        );
                        // Named by the same reverse lookup that button uses, so a place
                        // saved either way lands in the list reading as an address
                        // rather than as six decimals.
                        bookmarks.request_address(id, lat, lng);
                        *right_panel = RightPanel::Places;
                        context_menu.visible = false;
                    }
                }
                ui.separator();
            }

            if let Some(prim_id) = context_menu.hit_prim_id {
                // Clicked on an existing object.
                //
                // No key hints in these labels: `E` is *interact* (it was the
                // flight climb key once), `D` strafes right, and Escape closes
                // nothing — three hints that named the wrong key, on a surface a
                // phone reaches with no keyboard at all. Delete/Backspace does
                // delete a selection (`picking`), but that belongs beside the
                // selection, not on a menu entry.
                if ui.button("Edit object…").clicked() {
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

                if ui.button("Delete object").clicked() {
                    tracing::info!(prim_id, "deleting prim");
                    game_state.prims_to_delete.push(prim_id);
                    context_menu.visible = false;
                }

                // Scripts are server state, so both entries are online-only —
                // `--offline` has no one to store a script or fire a touch.
                if let Some(session) = online.as_ref() {
                    if ui
                        .button(format!("{}  Edit script…", icons::NOTE))
                        .on_hover_text("Write a Luau script that runs on this object")
                        .clicked()
                    {
                        script_editor.open(prim_id);
                        *right_panel = RightPanel::Script;
                        context_menu.visible = false;
                    }
                    if ui
                        .button("Touch")
                        .on_hover_text("Fire the object's on_touch handler")
                        .clicked()
                    {
                        session.send(
                            "touch prim",
                            cartopolis_core::NetMessage::TouchPrim {
                                request_id: 0,
                                prim_id,
                            },
                        );
                        context_menu.visible = false;
                    }
                }
            } else if context_menu.hit_region_id.is_some() {
                // Clicked on empty space with a region to put a prim in. The menu
                // now also opens where there is no region at all (for the routing
                // entries above, and so `--offline` has a map right-click), so
                // this can no longer assume one exists.
                if ui.button("Create object…").clicked() {
                    edit_dialog.prim_id = None;
                    edit_dialog.is_new = true;
                    edit_dialog.name = "Object".to_string();
                    edit_dialog.color = [0.5, 0.5, 0.5]; // neutral gray
                    edit_dialog.shape = "box".to_string();
                    edit_dialog.texture_id = None;
                    edit_dialog.texture_picker_open = false;
                    edit_dialog.path_cut_begin = 0.0;
                    edit_dialog.path_cut_end = 1.0;
                    edit_dialog.hollow = 0.0;
                    edit_dialog.warp = crate::resources::PrimWarp::default();
                    edit_dialog.surface = cartopolis_core::PrimSurface::default();
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

            ui.separator();
            if ui.button("Close").clicked() {
                context_menu.visible = false;
            }
        });
}

/// Width of the object editor and its texture picker, clamped to the viewport.
///
/// The floor is deliberately below anything comfortable: on a viewport narrower
/// than the floor the dialog is cut off either way, and a width that goes
/// negative is a panic in egui's layout rather than an ugly dialog.
fn dialog_width(viewport_w: f32, preferred: f32) -> f32 {
    preferred.min(viewport_w - 16.0).max(200.0)
}

/// The frame the object editor and the texture picker wear: the sign-in card's
/// recipe, which is what a themed window looks like in this app — a surface fill,
/// a hairline, the shared corner radius and real margins.
fn dialog_frame(style: &egui::Style) -> egui::Frame {
    egui::Frame::window(style)
        .fill(theme::SURFACE)
        .stroke(egui::Stroke::new(1.0, theme::LINE))
        .inner_margin(egui::Margin::symmetric(10, 10))
}

/// The object (prim) editor.
///
/// **It wears the panels' chrome, not egui's.** This is the primary surface of
/// the building tool — right-click anything in the world and it opens — and it
/// was the one window still carrying a stock title bar, collapse triangle and
/// close glyph, against nine right-hand panels wearing `theme::panel_header`'s
/// raised band, accent medallion and painted cross. The recipe is the sign-in
/// card's: `title_bar(false)`, a themed frame, and the shared header for the
/// title and the way out.
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

    // 320 pt of dialog does not fit a 274 pt phone viewport, and a `Window` is no
    // help there: it clamps its *position* to the screen, not its size. The Shape
    // tab is a good 700 pt tall on its own, so the height needs the same treatment
    // — `vscroll` below, bounded by this.
    let viewport = egui.viewport_pts();
    let width = dialog_width(viewport.x, 320.0);
    let max_height = (viewport.y - 24.0).max(200.0);
    let ctx = egui.ctx_mut();
    let mut dialog_open = true;

    let title = if edit_dialog.is_new {
        "Create object"
    } else {
        "Edit object"
    };

    Window::new(title)
        .id(egui::Id::new("prim_edit_dialog"))
        .title_bar(false)
        .default_width(width)
        .max_width(width)
        .max_height(max_height)
        .vscroll(true)
        .frame(dialog_frame(&ctx.style_of(ctx.theme())))
        .show(ctx, |ui| {
            ui.set_max_width(width);
            if theme::panel_header(ui, icons::TUNE, title) {
                dialog_open = false;
            }
            // Tab bar
            ui.horizontal(|ui| {
                // Not "Edit / Object / Texture": the dialog is *called* "Edit
                // object" now, so two of those three named the dialog rather than
                // the tab. These name what is behind them.
                ui.selectable_value(&mut edit_dialog.active_tab, 0, "Basics");
                ui.selectable_value(&mut edit_dialog.active_tab, 1, "Shape");
                ui.selectable_value(&mut edit_dialog.active_tab, 2, "Surface");
            });
            ui.separator();

            match edit_dialog.active_tab {
                // ── Basics tab ──────────────────────────────────────────────
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
                // ── Shape tab ───────────────────────────────────────────────
                1 => {
                    egui::Grid::new("object_grid")
                        .num_columns(2)
                        .spacing([8.0, 4.0])
                        .show(ui, |ui| {
                            ui.label("Building block type");
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
                    ui.label("Path cut begin and end");
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
                    ui.label("Twist begin and end");
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
                    ui.label("Top shear");
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
                    ui.label("Slice begin and end");
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
                // ── Surface tab ─────────────────────────────────────────────
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

                            ui.label("Full bright");
                            crate::systems::theme::checkbox(
                                ui,
                                &mut edit_dialog.surface.full_bright,
                                "",
                            );
                            ui.end_row();
                        });

                    ui.separator();

                    // Repeats Per Face (tiling count per axis, with optional flip).
                    ui.label("Repeats per face");
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
                    ui.label("Repeats per metre");
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

    // Texture picker window — the editor's chrome, for the same reason the editor
    // has it: two windows opened from one another, one themed and one stock, look
    // like two applications.
    if edit_dialog.texture_picker_open && edit_dialog.visible {
        let mut picker_open = edit_dialog.texture_picker_open;
        let mut clicked_texture: Option<String> = None;
        let picker_w = dialog_width(egui.viewport_pts().x, 380.0);
        let picker_h = (egui.viewport_pts().y - 24.0).max(200.0);
        let ctx = egui.ctx_mut();
        Window::new("texture library")
            .id(egui::Id::new("prim_texture_picker"))
            .title_bar(false)
            .default_width(picker_w)
            .max_width(picker_w)
            .max_height(picker_h)
            .frame(dialog_frame(&ctx.style_of(ctx.theme())))
            .show(ctx, |ui| {
                ui.set_max_width(picker_w);
                if theme::panel_header(ui, icons::PALETTE, "Texture library") {
                    picker_open = false;
                }
                egui::ScrollArea::vertical()
                    .max_height(400.0)
                    .auto_shrink([false, false])
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
            sess.send(
                "create prim",
                NetMessage::CreatePrim {
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
                },
            );
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
                sess.send(
                    "update prim",
                    NetMessage::UpdatePrim {
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
                    },
                );
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
            sess.send(
                "delete prim",
                NetMessage::DeletePrim {
                    request_id: 0,
                    prim_id,
                },
            );
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
    surface: &cartopolis_core::PrimSurface,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The developer panel's number formatting. Small, but these run on every
    /// count in the panel and a grouping that misplaces a separator makes a
    /// triangle budget unreadable, which is the one job the panel has.
    #[test]
    fn counts_group_into_threes_from_the_right() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000), "1\u{202f}000");
        assert_eq!(thousands(12_345), "12\u{202f}345");
        assert_eq!(thousands(1_234_567), "1\u{202f}234\u{202f}567");
    }

    /// An empty scene is the state the panel opens in, so the shadow-share row
    /// must not be a NaN on frame one.
    #[test]
    fn a_share_of_nothing_is_zero_not_a_nan() {
        assert_eq!(share(0, 0), 0.0);
        assert_eq!(share(1, 4), 25.0);
    }

    /// The frame overlay must start below everything the top-left disc column
    /// draws, because those are floating `Area`s and they paint over it. This is
    /// the regression it is pinned against: the overlay's offset was a bare
    /// `40.0` from a menu bar that no longer exists, so the layers disc and the
    /// compass sat on top of the frame rate.
    #[test]
    fn the_frame_overlay_clears_the_top_left_disc_column() {
        let column = crate::systems::hud::left_column_bottom();
        assert!(
            column > crate::systems::dock::reserved_top(),
            "the column has to include what is under the layers disc"
        );
        // What `render_stats_overlay` anchors at.
        let overlay_top = column + theme::OSD_GROUP_GAP;
        assert!(overlay_top > column, "the overlay starts below the column");
    }

    #[test]
    fn altitude_switches_unit_where_the_digits_stop_fitting() {
        assert_eq!(metres(120.0), "120 m");
        assert_eq!(metres(9_999.0), "9999 m");
        assert_eq!(metres(10_000.0), "10 km");
        assert_eq!(metres(1_200_000.0), "1200 km");
    }

    /// **A bottom-anchored `Area` taller than the sizing pass allows settles at
    /// a wrong height and stays there.** egui lays an `Area` out once against
    /// `Spacing::default_area_size` (600×**400**), remembers the content's size,
    /// and uses that as the `max_rect` from then on. Overflow is meant to grow
    /// it back, but only if the content reports its real height — and a
    /// `ScrollArea` shrinks to the room it is given while a justified top-down
    /// layout hands the widgets past the bottom whatever is left. Container and
    /// content then agree on the truncated height: a *stable* wrong answer, so
    /// it neither flickers nor recovers.
    ///
    /// The main menu was ~470 pt of entries in a 400 pt sizing pass, and the four
    /// below the fold — Sign in…, the status readout, Developer tools and Exit —
    /// were simply absent. It reads as a menu that is merely short, which is how
    /// it survived a UI sweep aimed at this very card.
    ///
    /// This reproduces the mechanism rather than calling the system (which needs
    /// a `World`), and asserts the fix: the sizing pass gets the whole band, so
    /// the card ends up as tall as its content.
    #[test]
    fn a_tall_bottom_anchored_card_is_not_truncated_by_the_sizing_pass() {
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(700.0, 900.0));
        let rows = 24;
        let row = 24.0;
        let wanted = rows as f32 * row;
        assert!(
            wanted > 400.0,
            "the case only exists above the default sizing pass"
        );

        let run = |default_size: Option<egui::Vec2>| {
            let ctx = egui::Context::default();
            theme::install(&ctx);
            let mut last = 0.0;
            // Several frames: the failure is what it *settles* on, not frame one.
            for _ in 0..8 {
                let raw = egui::RawInput {
                    screen_rect: Some(screen),
                    ..Default::default()
                };
                let _ = ctx.run_ui(raw, |ui| {
                    let mut area = egui::Area::new(egui::Id::new("card"))
                        .order(egui::Order::Foreground)
                        .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -60.0));
                    if let Some(size) = default_size {
                        area = area.default_size(size);
                    }
                    area.show(ui.ctx(), |ui| {
                        egui::Frame::popup(ui.style()).show(ui, |ui| {
                            ui.set_width(MENU_WIDTH);
                            egui::ScrollArea::vertical()
                                .max_height(816.0)
                                .auto_shrink([false, true])
                                .show(ui, |ui| {
                                    ui.with_layout(
                                        egui::Layout::top_down_justified(egui::Align::Min),
                                        |ui| {
                                            for i in 0..rows {
                                                ui.add_sized(
                                                    egui::vec2(MENU_WIDTH, row),
                                                    egui::Button::new(format!("row {i}")),
                                                );
                                            }
                                        },
                                    );
                                });
                        });
                    });
                });
                last = ctx
                    .memory(|m| m.area_rect(egui::Id::new("card")))
                    .map_or(0.0, |r| r.height());
            }
            last
        };

        let starved = run(None);
        let fixed = run(Some(egui::vec2(MENU_WIDTH, 816.0)));
        assert!(
            starved < wanted * 0.9,
            "the bug should reproduce without a sizing hint: {starved} vs {wanted}"
        );
        assert!(
            fixed >= wanted,
            "with the band as the sizing pass the card must hold its content: \
             {fixed} vs {wanted}"
        );
    }

    /// The menu card is a size-to-content `Area`, so nothing clamps it: what it
    /// asks for is what it takes, off the edge of the screen included. This is
    /// the number that decides whether the app menu — the only route to
    /// Settings, sign-in and Exit — is on the screen at all. The card it
    /// replaced folded half its contents away below this width; a card that is
    /// the same on every viewport has to fit the smallest one instead.
    #[test]
    fn the_menu_card_fits_a_phone() {
        // 411 dp at `ui_scale` 1.5, i.e. what `quality::Tier::Phone` resolves an
        // ordinary Android handset to.
        let viewport = 274.0;
        // `Frame::popup`'s margins and the card's stroke, generously.
        let card = MENU_WIDTH + 24.0;
        assert!(
            card <= viewport,
            "menu card {card} does not fit {viewport} pt"
        );
    }

    /// The object editor asks for 320 pt, which is wider than a phone. A
    /// `Window` clamps its position to the screen but never its width, so this
    /// is the only thing keeping the Save/Cancel row reachable.
    #[test]
    fn the_object_editor_is_clamped_to_the_viewport() {
        assert_eq!(dialog_width(1280.0, 320.0), 320.0);
        assert_eq!(dialog_width(274.0, 320.0), 258.0);
        assert_eq!(dialog_width(274.0, 380.0), 258.0);
        // …and never negative, whatever the viewport claims to be.
        assert!(dialog_width(0.0, 320.0) > 0.0);
    }
}
