//! An external GNSS receiver, read over a byte stream — an RTK rover such as
//! the Unicore UM980 — with RTCM corrections from an NTRIP caster written back
//! into it (`docs/notes/rtk-receiver.md`).
//!
//! **The receiver does the RTK; this crate keeps it fed and listens.** An RTK
//! solution needs a base station's observations, which arrive from a caster
//! over the internet and must reach the receiver within a second or two. The
//! receiver then reports a position that is centimetres rather than metres,
//! and says how sure it is. Nothing here solves anything.
//!
//! * [`nmea`] — framing and the three sentences a position needs;
//! * [`solution`] — one epoch assembled from them, with its accuracy;
//! * [`rtcm`] — enough RTCM 3 framing to know the corrections are real;
//! * [`ntrip`] — the caster protocol, and the nearest mountpoint;
//! * [`receiver`] — the two threads that run it all over a [`receiver::Port`];
//! * `serial` — the desktop's port (feature `serial`). Android's is in the
//!   client, being the framework's USB host API rather than a tty.

pub mod nmea;
pub mod ntrip;
pub mod receiver;
pub mod rtcm;
#[cfg(feature = "serial")]
pub mod serial;
pub mod solution;

pub use nmea::FixQuality;
pub use ntrip::Caster;
pub use receiver::{CasterState, Config, Link, Opener, Port, Receiver, Status};
pub use solution::Solution;
