//! Registration for `systems::net`: the sim transport, private chats and calls,
//! the session driver, voice and the user store's restore/autosave. The phase
//! each system runs in is `crate::phase`'s; the edges beside them are the ones
//! `lib.rs` carried before the split.

use bevy::prelude::*;

use crate::phase::ClientPhase;
use crate::systems;
use crate::systems::{avatar, network};

pub struct NetPlugin;

impl Plugin for NetPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<systems::private_chat::PrivateInbox>()
            .init_resource::<systems::private_chat::PrivateChats>()
            .init_resource::<systems::calls::CallInbox>()
            .init_resource::<systems::calls::Calls>()
            .init_resource::<systems::contacts::ContactsInbox>()
            .init_resource::<systems::contacts::Contacts>()
            .init_resource::<systems::voice::VoiceState>();
        // Headless call tests (`CARTO_CALL`, `CARTO_ANSWER`): writes the request
        // `drive_calls` takes, so it runs first.
        #[cfg(not(target_os = "android"))]
        app.add_systems(
            Update,
            systems::calls::drive_call_harness
                .before(systems::calls::drive_calls)
                .in_set(ClientPhase::Input),
        );
        // Where the simulator wakes this phone for a call while the app is closed.
        #[cfg(target_os = "android")]
        app.add_systems(
            Update,
            systems::push::send_push_endpoint.in_set(ClientPhase::Input),
        );
        // Voice plays through the output device the soundscape already opened, as a
        // custom `Decodable` asset. Registering the type is what teaches bevy_audio
        // to build a rodio source from it; without this an `AudioPlayer<VoiceStream>`
        // is a component nothing looks at, and voice is silent with nothing logged.
        #[cfg(feature = "audio")]
        {
            use bevy::audio::AddAudioSource;
            app.add_audio_source::<systems::voice::VoiceStream>();
        }
        // Read by `run_client` right after this plugin is added: the launch's
        // session request is written into it before the first frame.
        app.init_resource::<systems::session::SessionState>();

        app.add_systems(
            Update,
            (
                network::apply_network_snapshot,
                network::create_egui_texture_handles.after(network::apply_network_snapshot),
                // After the network system has sorted this frame's private-chat
                // traffic into the inbox it drains.
                systems::private_chat::drive_private_chats.after(network::apply_network_snapshot),
                // After the chats: a call reads the chat's epoch, which a commit
                // applied there this frame has just moved.
                systems::calls::drive_calls.after(systems::private_chat::drive_private_chats),
                // Contacts, between the two: its first messages become private-chat
                // starts the same frame their request is on record.
                systems::contacts::drive_contacts
                    .after(network::apply_network_snapshot)
                    .before(systems::private_chat::drive_private_chats),
                // The session — which world this client is in (`systems::session`).
                // The driver runs after the sign-in screen so a mode picked this
                // frame is applied this frame: a sign-in that visibly waits a frame
                // for its own spinner reads as a stutter on the one screen whose
                // whole job is to say what is happening.
                systems::session::drive_session.after(systems::login_screen::render_login_screen),
                // After the driver, which is what adds and removes `OnlineSession`
                // — half of the want this reconciles. Ordered so that leaving a
                // session takes the service down on the same frame rather than
                // leaving a "connected" notification over a client that isn't.
                systems::background::apply_background_connection
                    .after(systems::session::drive_session),
                // After the driver marks a session as come back, and after the
                // avatar it brings back is in the grid, so the resume lands on
                // the same frame the avatar appears.
                systems::session::keep_avatar_across_reconnect
                    .after(systems::session::drive_session)
                    .after(crate::attach_avatar_to_bigspace)
                    .before(systems::geo_nav::apply_teleport),
                // Voice capture. Two systems rather than one because the *other*
                // writer of `talk_held` is the chat panel's Hold-to-talk button,
                // the only way to talk on a phone, so the key reader must not be
                // the thing that clears a flag it does not own. Capture reads the
                // panel's press from the frame before: 16 ms of latency on a
                // control a finger is holding, against threading a frame-order
                // constraint through the whole panel chain.
                //
                // Not behind the `audio` feature. `AudioInput::start` fails
                // cleanly in a build without it and the failure is shown, which
                // is the honest shape — the alternative is a Voice control that is
                // simply absent with no explanation.
                systems::voice::read_talk_key,
                systems::voice::drive_voice_capture
                    .after(systems::voice::read_talk_key)
                    .after(network::apply_network_snapshot),
                // Restore feeds pending_teleport, so it must run before the
                // teleport applies; teleports move the camera, so apply before
                // camera_controls reads it this frame.
                // All skipped under `--shot`: a scripted capture places its own
                // camera and must not overwrite the interactive session's location.
                (
                    systems::user_store::restore_start_location
                        .after(systems::map_stream::init_map_stream)
                        .run_if(not(systems::shot_harness::harness_active)),
                    // Same gate: a scripted capture's layer state comes from its
                    // manifest (`--ui`), never from what a session left in user.json.
                    systems::user_store::restore_layer_prefs
                        .run_if(not(systems::shot_harness::harness_active)),
                    // And the same for the sun/weather switchers: a capture's sky
                    // comes from `--time`/`--fog`/`--clouds`/`--rain`, not from what
                    // the last interactive session was looking at.
                    systems::user_store::restore_sky_prefs
                        .run_if(not(systems::shot_harness::harness_active)),
                )
                    // All three read the loaded document, and the autosave below
                    // overwrites it — on the first frame, unordered against them
                    // until this edge existed. See `user_store::Restore`.
                    .in_set(systems::user_store::Restore),
            )
                .in_set(ClientPhase::Input),
        )
        .add_systems(
            Update,
            network::send_network_intent
                .after(avatar::handle_avatar_movement)
                .in_set(ClientPhase::Move),
        )
        .add_systems(
            Update,
            (
                // The call bar: a call ringing, or the one you are in. Its own
                // `Foreground` area like the sharing notice it sits under, so it
                // is not in the panel chain; after the pins only so it is drawn
                // over them.
                systems::calls::render_call_bar.after(systems::place_pins::render_place_pins),
                systems::user_store::autosave_user_store
                    // Never before a restore has read the document this overwrites:
                    // its first deadline is time zero, so without this it wrote the
                    // launch defaults over the saved sky and layers on frame one and
                    // the restores then read back its own output.
                    .after(systems::user_store::Restore)
                    .run_if(not(systems::shot_harness::harness_active)),
            )
                .in_set(ClientPhase::Present),
        );

        // The output half, behind the `audio` feature with the rest of the
        // soundscape (ALSA headers on Linux — see Cargo.toml).
        #[cfg(feature = "audio")]
        app.add_systems(
            Update,
            (
                // Other people's voices. Here because it spawns `AudioPlayer`s and
                // is therefore as feature-bound as the soundscape — the *receive*
                // half is not: frames are decoded into a ring by the network drain
                // whatever this build can play, so a no-audio build still reports
                // who is talking rather than pretending nobody is.
                systems::voice::update_voice_playback.after(network::apply_network_snapshot),
                // A call's voices, through the same output.
                systems::calls::play_calls.after(systems::calls::drive_calls),
            )
                .in_set(ClientPhase::Present),
        );
    }
}
