//! The client's Model Context Protocol server, outside the client crate.
//!
//! The server half of `systems::dev::mcp` (see that module's note for the
//! design, and `docs/notes/driving-the-client-from-an-ide.md` for what it
//! costs): a tokio runtime on a thread of its own, a Unix domain socket, and the
//! rmcp tool surface. **It touches no ECS.** Every tool builds a
//! [`protocol::Job`], pushes it down a channel and awaits the oneshot; the
//! client's `mcp::pump` drains the channel in `Update` and answers from the
//! world. That channel was already the whole boundary, which is what lets this
//! be its own crate: the heavy generic code (rmcp's `#[tool]` expansions and
//! its transport) compiles here and no longer rides along with every edit to the
//! Bevy client.
//!
//! What crosses the channel lives here too — [`protocol`] for the camera calls
//! and [`args`] for what the signed-in account is asked to do — so the client
//! depends on this crate and never the other way round. This crate must not
//! depend on `bevy` or on `cartopolis`.

pub mod args;
mod guide;
pub mod protocol;
/// Unix only: the listener is a Unix domain socket, and Windows has no MCP for
/// now (`docs/notes/driving-the-client-from-an-ide.md`).
#[cfg(unix)]
pub mod server;
/// The voice agent's tools: these, through an in-process MCP session.
#[cfg(unix)]
pub mod voice_tools;

pub use guide::scripting_guide;
