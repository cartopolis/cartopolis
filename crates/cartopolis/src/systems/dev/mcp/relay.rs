//! `cartopolis mcp [--socket PATH]`: an editor's stdio, piped into a running
//! client's MCP socket.
//!
//! An editor launches an MCP server as a command and talks to it over stdin and
//! stdout; the server this client hosts is a Unix socket ([`super::server`]).
//! This is the whole of the bridge between them — bytes in both directions,
//! with the framing (one JSON-RPC message per line) the same on both sides, so
//! nothing here parses anything. It is also what makes a client on another
//! machine reachable: `ssh HOST cartopolis mcp` is the same pipe with ssh's
//! authentication in front of it, and a bot run on a build machine is reached
//! exactly that way.
//!
//! **Nothing but the protocol may reach stdout**, so this runs from the very
//! top of [`crate::run`], before the logger, the watchdog, the worker pool or a
//! single Bevy type exists. Plain blocking std: two threads and two
//! `io::copy`s, and no runtime to start.

use clap::Parser;
use std::io::{self, Write as _};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

/// Pipe stdio to a running Cartopolis client's MCP socket.
///
/// Configure an editor with `{"command": "/path/to/cartopolis", "args":
/// ["mcp"]}` (Settings ▸ General ▸ Editor access shows the exact line), or
/// reach another machine's client with `ssh HOST cartopolis mcp`.
#[derive(Parser, Debug)]
#[command(name = "cartopolis mcp")]
struct RelayArgs {
    /// The socket the client serves on. Defaults to where a client serves
    /// unless told otherwise: `$XDG_RUNTIME_DIR/cartopolis/mcp.sock`, else
    /// `$TMPDIR` or `/tmp` + `cartopolis-$UID/mcp.sock`.
    #[arg(long, value_name = "PATH")]
    socket: Option<PathBuf>,
}

/// Is this launch the relay? `cartopolis mcp …`, and only as the first
/// argument — `cartopolis --offline mcp` is a client launch with a stray word.
pub fn requested() -> bool {
    std::env::args_os().nth(1).is_some_and(|a| a == "mcp")
}

/// Run the relay and exit. 0 when either side closed cleanly; 1, with one line
/// on stderr, when there is no client to reach or the pipe broke.
pub fn main() -> ! {
    // clap takes the first item as the program name, and that is what usage
    // text shows — so the binary and the word `mcp` become one item.
    let args = RelayArgs::parse_from(
        std::iter::once(std::ffi::OsString::from("cartopolis mcp"))
            .chain(std::env::args_os().skip(2)),
    );
    let path = args.socket.unwrap_or_else(super::default_socket_path);

    let socket = match UnixStream::connect(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "no Cartopolis is serving MCP at {} — is it running with MCP on? ({e})",
                path.display()
            );
            std::process::exit(1);
        }
    };

    // stdin → socket on a thread of its own. When the editor closes stdin the
    // write half is shut, which the server reads as the end of the session and
    // answers by closing its side — which ends the copy below.
    let mut upstream = match socket.try_clone() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cannot share the socket between directions: {e}");
            std::process::exit(1);
        }
    };
    std::thread::spawn(move || {
        let _ = io::copy(&mut io::stdin().lock(), &mut upstream);
        let _ = upstream.shutdown(std::net::Shutdown::Write);
    });

    // socket → stdout here. Its end is the session's end: the client stopped,
    // or answered the closed stdin by closing too. The stdin thread may still be
    // blocked in a read, and `exit` is what ends it.
    let mut downstream = socket;
    let mut stdout = io::stdout().lock();
    let copied = io::copy(&mut downstream, &mut stdout).and_then(|_| stdout.flush());
    match copied {
        Ok(()) => std::process::exit(0),
        // The editor went away mid-answer; nobody is left to tell.
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => std::process::exit(0),
        Err(e) => {
            eprintln!("the MCP connection to {} broke: {e}", path.display());
            std::process::exit(1);
        }
    }
}
