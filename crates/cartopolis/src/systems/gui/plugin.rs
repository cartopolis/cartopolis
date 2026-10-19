//! Registration for `systems::gui`: the dock and cards, the right-hand panel
//! chain, the sign-in screen, the prim edit dialog, the layer toggles, the
//! on-screen keyboard and controls. The phase each system runs in is
//! `crate::phase`'s; the edges beside them are the ones `lib.rs` carried before
//! the split.

use bevy::prelude::*;

use crate::phase::ClientPhase;
use crate::systems;
use crate::systems::stream_watch::WatchStreamExt;

pub struct GuiPlugin {
    /// `--aerial`: the only way into the aerial layer in a scripted run. An
    /// interactive session ticks it in the Layers panel (or restores it from
    /// `data/user.json`, which `apply_prefs` does after this). The measured
    /// ground needs no such switch any more — it is always on.
    pub aerial: bool,
    /// `--sewers`, for the same reason.
    pub sewers: bool,
    /// `--underground`: layer keys, or `all`.
    pub underground: Vec<String>,
    /// `--satellites`, for the same reason as `--sewers`.
    pub satellites: bool,
}

impl Plugin for GuiPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<systems::settings::SettingsState>()
            // The dock's mark disc and the card it opens (`systems::ui`).
            .init_resource::<systems::ui::MainMenu>()
            // The place card a picked search result opens (`systems::place_card`).
            .init_resource::<systems::place_card::PlaceCard>()
            .init_resource::<systems::onscreen_keyboard::OnScreenKeyboard>()
            // What the on-screen navigation controls asked for this frame
            // (`systems::map_osd`); read by `free_camera::camera_controls`.
            .init_resource::<systems::map_osd::OsdInput>()
            .insert_resource({
                let mut layers = systems::data_layers::DataLayers::default();
                layers.aerial_enabled = self.aerial;
                layers.sewers = self.sewers;
                layers.satellites_enabled = self.satellites;
                for (slot, def) in layers
                    .underground
                    .iter_mut()
                    .zip(systems::underground::LAYERS.iter())
                {
                    *slot = self.underground.iter().any(|k| k == "all" || k == def.key);
                }
                layers
            })
            .watch_stream::<systems::data_layers::DataLayers>()
            .init_resource::<systems::bookmarks::BookmarksState>()
            .init_resource::<systems::chat::ChatState>()
            .init_resource::<systems::chat_overlay::ChatOverlay>()
            .init_resource::<systems::chats::ChatsView>()
            .init_resource::<systems::communities::Communities>()
            .init_resource::<systems::faces::Faces>()
            .init_resource::<systems::script_editor::ScriptEditorState>()
            .init_resource::<systems::model_library::ModelLibraryState>()
            // Drawn over everything until the first tiles are up. Mirrors the
            // splash in index.html, which covers the wasm load this cannot.
            .init_resource::<systems::splash::BootSplash>()
            .init_resource::<systems::chats::ScreenRestore>()
            .init_resource::<systems::camera_screen::CameraScreen>()
            .init_resource::<systems::admin::AdminState>()
            .init_resource::<systems::access_tokens::AccessTokens>()
            .init_resource::<systems::calendar::Calendar>();

        // The calendar (`systems::calendar`): its book takes in what the chats
        // hold before the Community panel draws it, and its reminder is its own
        // `Foreground` area over everything, so not in the panel chain.
        app.add_systems(
            Update,
            (
                (
                    systems::calendar::collect_calendar,
                    systems::calendar::track_here,
                )
                    .in_set(ClientPhase::Input)
                    .before(systems::chats::render_chats_panel),
                systems::calendar::render_reminder
                    .after(systems::place_pins::render_place_pins)
                    .in_set(ClientPhase::Present),
            ),
        );

        // The camera (`systems::camera_screen`): in Mixed it drives the world
        // camera after any teleport has landed and before the camera system reads
        // the rotation, and its screen is drawn over everything, as the call
        // screen is — its own `Foreground` area, so not in the panel chain. A
        // picture for a post is handed to the composer after it.
        app.add_systems(
            Update,
            (
                systems::camera_screen::drive_camera
                    .after(systems::geo_nav::apply_teleport)
                    .before(systems::free_camera::camera_controls)
                    .in_set(ClientPhase::Anchor),
                systems::camera_screen::render_camera
                    .after(systems::place_pins::render_place_pins)
                    .in_set(ClientPhase::Present),
                systems::camera_screen::hand_picture_to_post.in_set(ClientPhase::Present),
                // The ring's Post here, and the chip that says only one
                // community's posts are on the map (`community_posts`): its own
                // `Area` under the switch, drawn after the switch it sits under.
                systems::community_posts::post_here.in_set(ClientPhase::Present),
                // A text field: drawn before the on-screen keyboard reads it.
                systems::community_posts::render_floating_composer
                    .after(systems::community_posts::post_here)
                    .before(systems::onscreen_keyboard::render_onscreen_keyboard)
                    .in_set(ClientPhase::Present),
                systems::community_posts::render_filter_chip
                    .after(systems::chats::render_chats_switch)
                    .in_set(ClientPhase::Present),
            ),
        );

        // People's pictures (`systems::faces`): the table the discs read is
        // rewritten before any panel paints one, and the polaroid's viewfinder
        // is its own `Foreground` area over the map, so not in the panel chain.
        app.add_systems(
            Update,
            (
                systems::faces::drive_faces
                    .before(systems::ui::render_main_menu)
                    .in_set(ClientPhase::Input),
                systems::faces::render_face_viewfinder
                    .after(systems::place_pins::render_place_pins)
                    .in_set(ClientPhase::Present),
            ),
        );

        // Chats or World, and the open chat, kept across Android stopping the
        // app (`chats::remember_screen`). Before the panels, so a restored tab
        // is drawn on the frame it is restored.
        app.add_systems(
            Update,
            (
                systems::chats::restore_screen,
                systems::chats::remember_screen,
                systems::chats::apply_sign_out,
                systems::communities::sync_device,
                // Public posts' answers, and the camera's view for the one
                // being written.
                systems::community_posts::answer_post_view,
            )
                .chain()
                .in_set(ClientPhase::Input)
                .before(systems::chats::render_chats_panel),
        );

        // Drop egui geometry for the screenshot frame, after end_egui_frame has
        // written it.
        app.add_systems(
            PostUpdate,
            systems::ui::suppress_egui_for_screenshot.after(systems::egui_manager::end_egui_frame),
        );

        app.add_systems(
            Update,
            (
                // The bottom dock and the top-left layers disc — floating `Area`s
                // of round panel toggles, so they reserve no root-`Ui` space and
                // stay out of the panel chain. **Before the main menu**, which is
                // not a preference: the dock owns the mark disc that toggles the
                // menu, and a menu drawn before its own switch would open a frame
                // late and dismiss itself on the click that opened it.
                systems::dock::render_dock.before(systems::ui::render_main_menu),
                // The place card — a floating `Area` above the dock, so it stays
                // out of the panel chain too. **Before the panels**: taking a pick
                // out of `GeoNavState::pending_card` is also what closes the Search
                // panel, and doing it before the chain runs means the panel never
                // draws a frame over the card it just opened.
                systems::place_card::render_place_card.before(systems::ui::render_main_menu),
                // The building card, on the same terms and for the same reasons: a
                // floating `Area` above the dock, outside the panel chain,
                // suppressed by whatever owns the bottom of the screen. **After**
                // the place card, which is anchored to the same centre-bottom —
                // they are never both up (one comes from a search pick, the other
                // from a click on the map) and the order makes that definite rather
                // than a race if they ever are.
                systems::building_card::render_building_card
                    .after(systems::place_card::render_place_card)
                    .before(systems::ui::render_main_menu),
                // The floor picker (`systems::cutaway`): a floating column at the
                // right edge, outside the panel chain on the card's terms. After
                // the card, whose Look inside it answers in the same frame.
                systems::floor_picker::render_floor_picker
                    .after(systems::building_card::render_building_card)
                    .before(systems::ui::render_main_menu),
                // The menu a tap on a satellite opens, and the chip that says what the
                // camera is riding: both floating, outside the panel chain on the
                // card's terms. The menu first, so attaching from it puts the chip up
                // in the same frame.
                systems::satellites::render_satellite_menu
                    .after(systems::building_card::render_building_card)
                    .before(systems::ui::render_main_menu),
                systems::camera_attach::render_attach_chip
                    .after(systems::satellites::render_satellite_menu)
                    .before(systems::ui::render_main_menu),
                // The sign-in screen (`systems::login_screen`). It draws into its
                // own floating `Area` (a modal must sit above the main menu, which
                // is itself a `Foreground` area), so it is deliberately *not* in
                // the panel chain — it never touches the shared root `Ui`. It still
                // runs after the main menu, which is where it is opened from; the
                // session driver it feeds runs after it (`net::plugin`).
                systems::login_screen::render_login_screen.after(systems::ui::render_main_menu),
                // The screenshot driver runs after the dev panel so a button click
                // is seen the same frame it's captured.
                systems::ui::drive_screenshots.after(systems::dev_panel::render_dev_panel),
                // Context menu and edit dialog.
                systems::ui::render_context_menu,
                // Follow selection changes from picking before the dialog draws /
                // live edits apply.
                systems::ui::sync_dialog_to_selection
                    .after(systems::picking::prim_picking)
                    .before(systems::ui::render_edit_dialog),
                systems::ui::render_edit_dialog,
                systems::ui::apply_live_prim_edits.after(systems::ui::render_edit_dialog),
                systems::ui::send_prim_mutations.after(systems::ui::render_edit_dialog),
            )
                .in_set(ClientPhase::Input),
        )
        .add_systems(
            Update,
            (
                // Drains finished model-library requests and the file picker. Not
                // in the panel, because both outlive it being open: an upload
                // started and then the panel closed must still land.
                systems::model_library::poll_model_library,
                systems::data_layers::update_data_layers,
                systems::data_layers::animate_stop_rings
                    .after(systems::data_layers::update_data_layers),
                // "Minimal mode" forces the layer + weather toggles off before
                // they're applied (so opening the Route panel strips the map the
                // same frame), then apply_feature_toggles writes visibility.
                systems::data_layers::apply_minimal_mode
                    .before(systems::data_layers::apply_feature_toggles)
                    .before(systems::data_layers::apply_feature_toggles_to_new),
                // The full sweep is the expensive one — four queries over every
                // building, window, 3DBAG mesh and stream tile — so it runs only
                // when the answer can have changed. Note the gate is a *value*
                // comparison, not `resource_changed::<DataLayers>()`:
                // `update_data_layers` mutably derefs the resource every frame, and
                // Bevy marks a resource changed on `DerefMut` rather than by value,
                // so `resource_changed` here would have been true forever and gated
                // nothing. It also has to cover two inputs that are not toggles at
                // all — the space-tier boolean and the anchor tile — or a descent
                // from orbit leaves the buildings hidden.
                systems::data_layers::apply_feature_toggles
                    .run_if(systems::data_layers::feature_toggles_changed)
                    .after(systems::osm_buildings::update_buildings)
                    .after(systems::map_stream::update_map_stream)
                    .after(systems::map_stream::update_tile_occlusion),
                // …and the entities that streamed in *this* frame get the current
                // state applied unconditionally, because the gate above is closed
                // on exactly the frames where nothing about the toggles changed but
                // a new building may still have arrived. A skipped system still
                // satisfies its ordering edges, so the `.after` below holds either
                // way.
                systems::data_layers::apply_feature_toggles_to_new
                    .after(systems::data_layers::apply_feature_toggles)
                    .after(systems::osm_buildings::update_buildings)
                    .after(systems::map_stream::update_map_stream)
                    .after(systems::map_stream::apply_stream_textures)
                    .after(systems::lod22::update_lod22),
                systems::data_layers::handle_poi_click
                    .after(systems::data_layers::update_data_layers),
                systems::bookmarks::poll_bookmark_addresses,
                // Not one of the ordered panel systems, and not gated on the panel
                // being open: a photo is captured while the user is in another app
                // entirely, and it has to land whatever the panel is doing when
                // they come back.
                systems::bookmarks::poll_place_photos,
            )
                .in_set(ClientPhase::Stream),
        )
        .add_systems(
            Update,
            // The on-screen navigation controls (`systems::map_osd`) — the verbs a
            // finger has no key for. Floating `Area`s, so like the dock, the HUD
            // and the keyboard it is deliberately *not* in the panel chain.
            //
            // Both links are load-bearing. It runs **after the drive tick**, which
            // is what decides whether driver mode owns the screen this frame, and
            // **before the camera**, so a held control is acted on in the frame it
            // was pushed — a frame of lag on a *held* control reads as the camera
            // lagging the thumb, and at 20 fps on a phone that is visible.
            systems::map_osd::render_map_osd
                .after(systems::driver::drive_driver_mode)
                .before(systems::free_camera::camera_controls)
                .in_set(ClientPhase::Camera),
        );

        // The right-hand panel chain — ONE ordered declaration.
        //
        // Every system here draws into the *same* shared root `Ui`, so their call
        // order fixes their right-to-left placement, and every one of them takes
        // `crate::systems::egui_manager::EguiMut`, so Bevy already runs them one at a time
        // regardless. The only thing ordering decides is *which* order — and where
        // two are unconstrained, Bevy is free to pick differently from frame to
        // frame.
        //
        // That is not a visual swap. The panels share the single `Background`
        // layer, so a panel that lands over another on some frames wins egui's hit
        // test on those frames, while `layer_id_at` still answers `Background`
        // because the contest is *within* a layer. The symptom is a checkbox that
        // ignores about half the clicks aimed at it, invisible to every
        // layer-level diagnostic and impossible to reproduce in a single-panel
        // test. `chat` and `appearance` were the two left unlinked once (fixed
        // 2026-07-31).
        //
        // This used to be ~12 `.after()` links scattered over seven places in two
        // `add_systems` calls, with the intended chain recorded only in a comment.
        // `.chain()` makes the order structural: a panel added to this tuple is
        // ordered by construction, and one added anywhere else is visibly not in
        // the chain — `egui_manager::every_root_ui_panel_is_in_the_chain` reads
        // the block between the two markers below and fails the build otherwise.
        // Since these systems cannot run concurrently anyway, the total order
        // costs no parallelism.
        //
        // **Two halves, one order.** The head runs in `ClientPhase::Input`: the
        // main menu opens the sign-in screen whose session driver has to land
        // before the teleport, and the route sheet's Start button feeds the drive
        // tick that steers the camera. The tail runs in `ClientPhase::Present`,
        // because it projects against the placed camera. Each half is chained and
        // the halves are chained to each other, so it is still one total order
        // (see `crate::phase`).
        //
        // `render_place_pins` must stay last but one: it culls against the root
        // `Ui`'s leftover space, which is only the map area once every side panel
        // has taken its width. `locate::render_locate_arrow` reads the same
        // leftover space and is ordered after it for the same reason.
        //
        // panel-chain:begin
        app.add_systems(
            Update,
            (
                (
                    systems::ui::render_main_menu,
                    systems::geo_nav::render_search_panel,
                    systems::data_layers::render_data_layers_panel,
                    systems::bookmarks::render_bookmarks_panel,
                    // The route planner draws a floating bottom sheet, not a docked
                    // panel (`navigation::SheetDetent`), so nothing about the shared
                    // root `Ui` puts it here. It stays in the chain because it owns
                    // two text fields and the on-screen keyboard anchors on the
                    // *end* of this chain to find them — registered outside it, the
                    // endpoint fields would be undrawn when the keys go looking for
                    // their rect.
                    systems::navigation::render_route_sheet,
                )
                    .chain()
                    .in_set(ClientPhase::Input),
                (
                    // The messenger: the chat list and one chat's Chat view
                    // (`systems::chats`). Its World view and the Chats | World
                    // switch are drawn over the map, after the composer below.
                    systems::chats::render_chats_panel,
                    // The Admin pages (`systems::admin`), in the same
                    // right-hand slot as the Community panel.
                    systems::admin::render_admin_panel,
                    systems::script_editor::render_script_panel,
                    systems::model_library::render_model_library_panel,
                    systems::ui::render_avatar_chooser,
                    systems::settings::render_settings_panel,
                    systems::settings::render_about_panel,
                    // Before the pins and after every panel, for the same reason
                    // both of those hold: it culls against the root `Ui`'s leftover
                    // space, which is only the map once the side panels have taken
                    // their width.
                    systems::interact::render_interaction_prompt,
                    // Chat over the map (`systems::chat_overlay`). In the chain
                    // because the bubbles cull against the root `Ui`'s leftover
                    // space exactly as the pins do; near the end because that space
                    // is only the map once every panel has taken its width. The
                    // composer draws last of the three so it is on top of the
                    // transcript it expands, and — being ordered before the
                    // on-screen keyboard — its field is registered in time for the
                    // keyboard to find it.
                    // And after the other people have been moved this frame: the
                    // two were unordered, so a balloon hung over where its speaker
                    // was on whichever frames the scheduler ran it first.
                    systems::chat_overlay::render_chat_bubbles
                        .after(systems::free_camera::camera_controls)
                        .after(systems::avatar::smooth_remote_avatars),
                    systems::chat_overlay::render_chat_transcript,
                    systems::chat_overlay::render_chat_composer,
                    // A chat's World view has a text field, so it is ordered before
                    // the on-screen keyboard for the composer's reason.
                    systems::chats::render_chat_world,
                    systems::chats::render_chats_switch,
                    // The person card and the first-message sheet
                    // (`systems::contacts`): the sheet is a text field too.
                    systems::contacts::render_person_card,
                    // Also after the camera has moved, so pins project onto this
                    // frame's view.
                    // And after the people it stands markers on have moved
                    // (`render_chat_bubbles` says why).
                    systems::place_pins::render_place_pins
                        .after(systems::free_camera::camera_controls)
                        .after(systems::avatar::smooth_remote_avatars),
                    // The device arrow: where the phone is, in the views where the
                    // avatar isn't standing in for it. Same placement rules as the
                    // pins above — it reads the root `Ui`'s leftover space to find
                    // the map area, so it must run after every panel, and after the
                    // camera so it projects onto this frame's view. It paints and
                    // never senses, so unlike a pin it cannot take a tap and needs
                    // no ordering against the dock beyond that.
                    systems::locate::render_locate_arrow
                        .after(systems::free_camera::camera_controls)
                        .after(systems::place_pins::render_place_pins),
                )
                    .chain()
                    .in_set(ClientPhase::Present),
            )
                .chain(),
        );
        // panel-chain:end

        app.add_systems(
            Update,
            (
                systems::data_layers::render_poi_popup
                    .after(systems::ui::render_main_menu)
                    .after(systems::free_camera::camera_controls),
                // The compass/locate column, which driver mode repurposes.
                systems::hud::render_map_controls
                    .after(systems::ui::render_main_menu)
                    .after(systems::driver::drive_driver_mode),
                // The on-screen keyboard — the only keyboard Android and a phone
                // browser have (`systems::onscreen_keyboard`). NativeActivity
                // brings up no IME at all, so every text field in the client can
                // be focused and cannot be typed into, which on a routing client
                // means the address bar and the geo search: the front door.
                //
                // It draws into a floating `Area`, never the shared root `Ui`, so
                // it is deliberately *not* in the panel chain — same as `dock` and
                // the HUD.
                //
                // But it **must run after every system that can draw a text
                // field**, and that is not a preference. egui surrenders a focused
                // widget's focus on a click landing anywhere else, so tapping a key
                // unfocuses the field, and putting that focus back in the same
                // frame is most of what this system does. Registered earlier, the
                // surrender happens *after* the restore and undoes it: the keyboard
                // types one character into nothing and hides itself. Reading the
                // focused field's rect — to decide whether to dock to the bottom or
                // flip to the top — needs the same ordering, because
                // `read_response` only sees widgets already drawn this pass.
                //
                // Three anchors, because the fields are spread over three places:
                // the panel chain (geo search, chat, assistant, bookmark rename,
                // route), the sign-in card, and the prim edit dialog.
                systems::onscreen_keyboard::render_onscreen_keyboard
                    .after(systems::place_pins::render_place_pins)
                    .after(systems::login_screen::render_login_screen)
                    .after(systems::ui::render_edit_dialog),
                // Escape's single dismissal ladder. It reads state every panel has
                // already written this frame, so it runs after the panel chain —
                // which used to hold only because it was registered after it; the
                // edge to the chain's last system says so now.
                systems::dismiss::handle_escape.after(systems::locate::render_locate_arrow),
                // The clock every chat line is aged against. Its own system rather
                // than a line at the top of one of the drawing ones, which return
                // early on half a dozen conditions — a clock that stops whenever
                // nothing is drawn makes every line on screen immortal. A frame's
                // lag against the network apply is deliberate and irrelevant: the
                // shortest thing measured off it is a 1.5 s fade.
                systems::chat_overlay::advance_chat_clock,
                systems::splash::boot_splash,
            )
                .in_set(ClientPhase::Present),
        );
    }
}
