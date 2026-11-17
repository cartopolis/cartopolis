//! Telling the simulator where to wake this phone when its app is closed
//! (protocol 29, `docs/notes/calls.md`, `docs/notes/message-wake-ups.md`).
//! The places it wakes it for plans near are `plans::send_plan_follows`'s.
//!
//! The endpoint comes from the phone's UnifiedPush distributor (ntfy, say),
//! through `Push.java` and `PushReceiver.java`, and waits in the app's
//! preferences (`platform::android::push_endpoint`). This hands it to the
//! server once per connection, and again whenever the distributor gives a new
//! one. Nothing more: the push the server sends says only "a call", "a
//! message" or "a plan" (`PushReceiver.java` shows which), and the ringing
//! itself is the roster this client already shows once connected.

use bevy::prelude::*;
use cartopolis_core::NetMessage;

use crate::resources::OnlineSession;

/// How often the preferences are read for a new endpoint. A distributor hands
/// one out rarely (install, re-registration), so this is about noticing, not
/// about speed.
const LOOK_EVERY_S: f32 = 10.0;

/// In front of the address, it tells the server this app shows a message and a
/// plan wake-up as what they are rather than as "Incoming call"
/// (`simulator::push::PLANS_PREFIX`). The server speaks this client's protocol
/// or none, so it always knows the prefix.
const PLANS_PREFIX: &str = "plans:";

/// Send the endpoint when there is one the server has not been told on this
/// connection.
pub fn send_push_endpoint(
    online: Option<Res<OnlineSession>>,
    time: Res<Time<Real>>,
    mut sent: Local<Option<String>>,
    mut next_look: Local<f32>,
) {
    let Some(online) = online else {
        // A new connection is a new server-side record of this device.
        *sent = None;
        *next_look = 0.0;
        return;
    };
    if online.is_added() {
        *sent = None;
        *next_look = 0.0;
    }
    let now = time.elapsed_secs();
    if now < *next_look {
        return;
    }
    *next_look = now + LOOK_EVERY_S;
    let endpoint = crate::platform::android::push_endpoint().map(|e| format!("{PLANS_PREFIX}{e}"));
    if endpoint.is_some() && endpoint != *sent {
        online.send(
            "push endpoint",
            NetMessage::SetPushEndpoint {
                endpoint: endpoint.clone(),
            },
        );
        tracing::info!("push endpoint registered with the simulator");
        *sent = endpoint;
    }
}
