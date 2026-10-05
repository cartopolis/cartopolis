//! The simulator's integration tests: ten suites that boot a real server over a
//! real socket, in **one** test binary.
//!
//! They used to be ten files directly under `tests/`, which is ten separate
//! integration-test *targets*, and cargo compiles and links one binary per
//! target. Each was ~36 MB of the same statically-linked server — 365 MB linked
//! on every build of the suite to run 58 tests — and `cargo test` then ran the
//! ten binaries **one after another**, so their startup and their deliberate
//! waits could not overlap. As one target there is one link, and libtest runs
//! all 58 on its own thread pool.
//!
//! Nothing here shares process state, which is what makes that safe and is worth
//! keeping true: every suite binds `127.0.0.1:0` for an ephemeral port and opens
//! its world in its own `tempfile::TempDir`, and none of them installs a global
//! logger or writes an environment variable. A new suite that needs any of those
//! belongs in its own target again, with a note saying why.
//!
//! Test names gain their module as a prefix — `authoring::a_prim_is_created`
//! rather than a bare `a_prim_is_created` — so `cargo test authoring::` runs one
//! suite the way naming the old binary used to.

mod auth_handshake;
mod authoring;
mod calls;
mod chat;
mod chat_pictures;
mod connection_teardown;
mod http_surface;
mod model_library;
mod moderation;
mod physical_prims;
mod presence;
mod prim_video;
mod private_chat;
mod scripting_wire;
mod voice;
mod web_mirror;
