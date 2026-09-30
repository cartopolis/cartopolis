//! Telling the simulator where to wake this phone when its app is closed
//! (protocol 29, `docs/notes/calls.md`).
//!
//! The endpoint comes from the phone's UnifiedPush distributor (ntfy, say),
//! through `Push.java` and `PushReceiver.java`, and waits in the app's
//! preferences (`platform::android::push_endpoint`). This hands it to the
//! server once per connection, and again whenever the distributor gives a new
//! one. Nothing more: the push the server sends is a content-free wake-up, and
//! the ringing itself is the roster this client already shows once connected.

use bevy::prelude::*;
use cartopolis_core::NetMessage;

use crate::resources::OnlineSession;

/// How often the preferences are read for a new endpoint. A distributor hands
/// one out rarely (install, re-registration), so this is about noticing, not
/// about speed.
const LOOK_EVERY_S: f32 = 10.0;

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
    let endpoint = crate::platform::android::push_endpoint();
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
