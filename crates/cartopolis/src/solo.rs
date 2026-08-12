//! Solo/dev mode (ADR-016): run an in-process `simulator` on loopback and connect
//! to it as a normal authenticated client. Feature-gated (`solo`, default on).
//!
//! There is deliberately no in-memory bypass — the embedded server binds a real
//! loopback TCP socket and the client goes through the same `connect` + auth path
//! `--connect` uses, so external connect stays first-class and always-tested.
//!
//! Startup is **asynchronous** because the sign-in screen switches modes while the
//! app is running: opening `data/world.db` and running its migrations is tens of
//! milliseconds of disk work, and doing it inline would drop frames of the very
//! modal that is supposed to be showing a spinner. [`start`] hands back a
//! [`SoloPending`] the session driver polls once a frame instead.

use std::sync::mpsc;
use std::sync::{Arc, Mutex};

/// Dev account solo mode provisions and the client logs in as (matches
/// `network::connect_credentials` defaults).
pub const DEV_USER: &str = "dev";
pub const DEV_PASSWORD: &str = "dev";

/// A running in-process sim. **Dropping this stops it** — the server thread is
/// parked on the other end of the channel, so a drop wakes it, drops the
/// `SimHandle` (aborting the tick and accept tasks) and lets the runtime shut
/// down. That matters because a session can be switched at runtime now: leaving
/// the old sim listening on its ephemeral port after signing in to a remote one
/// would leak a tokio runtime and a SQLite handle per switch.
///
/// The channel ends are wrapped in a `Mutex` for one reason: a Bevy `Resource`
/// must be `Sync`, and `mpsc`'s halves are `Send` but not `Sync`. Nothing here is
/// ever contended — `SessionState` owns both — so the lock costs nothing.
pub struct SoloSim {
    _shutdown: Mutex<mpsc::Sender<()>>,
}

/// A sim that is still booting. Poll it each frame with [`SoloPending::poll`].
pub struct SoloPending {
    rx: Mutex<mpsc::Receiver<Result<String, String>>>,
    /// Handed to the caller together with the address, so the sim outlives this.
    sim: Option<SoloSim>,
}

impl SoloPending {
    /// `None` while still starting; then the bound `host:port` plus the handle
    /// that keeps the sim alive, or the error that killed it.
    pub fn poll(&mut self) -> Option<Result<(String, SoloSim), String>> {
        let received = match self.rx.get_mut() {
            Ok(rx) => rx.try_recv(),
            Err(_) => return Some(Err("solo sim channel poisoned".into())),
        };
        match received {
            Ok(Ok(addr)) => Some(match self.sim.take() {
                Some(sim) => Ok((addr, sim)),
                // Only reachable if `poll` is called again after it already
                // handed the handle over; the sim is running but unowned.
                None => Err("solo sim polled after it was taken".into()),
            }),
            Ok(Err(e)) => Some(Err(e)),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => {
                Some(Err("solo sim thread exited before binding".into()))
            }
        }
    }
}

/// Start an in-process sim on an ephemeral loopback port and ensure the dev
/// account exists. Returns immediately; the address arrives through the returned
/// [`SoloPending`].
pub fn start() -> SoloPending {
    let (tx, rx) = mpsc::channel::<Result<String, String>>();
    let (shutdown_tx, shutdown_rx) = mpsc::channel::<()>();
    let spawned = std::thread::Builder::new()
        .name("solo-sim".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = tx.send(Err(format!("solo runtime: {e}")));
                    return;
                }
            };
            let handle = rt.block_on(async {
                let mut config = cartopolis_simulator::SimConfig::default();
                config.listen = "127.0.0.1:0".into(); // ephemeral — no clash with a manual sim
                let config = Arc::new(config);

                if let Err(e) = ensure_dev_account(&config.database_path) {
                    return Err(format!("solo dev account: {e:#}"));
                }
                cartopolis_simulator::serve(config)
                    .await
                    .map_err(|e| format!("solo serve: {e:#}"))
            });
            let handle = match handle {
                Ok(handle) => handle,
                Err(e) => {
                    let _ = tx.send(Err(e));
                    return;
                }
            };
            if tx.send(Ok(handle.local_addr.to_string())).is_err() {
                return; // caller gave up before we bound
            }
            // Park until the `SoloSim` is dropped. The runtime's worker threads
            // keep serving while this one blocks — a `Runtime` drives spawned
            // tasks for as long as it is alive, not only inside `block_on`.
            let _ = shutdown_rx.recv();
            drop(handle);
        });
    if let Err(e) = spawned {
        // Report through the same channel the caller already polls, so thread
        // spawn failure and serve failure surface identically in the UI.
        let (tx, rx_failed) = mpsc::channel();
        let _ = tx.send(Err(format!("solo sim thread: {e}")));
        return SoloPending {
            rx: Mutex::new(rx_failed),
            sim: None,
        };
    }
    SoloPending {
        rx: Mutex::new(rx),
        sim: Some(SoloSim {
            _shutdown: Mutex::new(shutdown_tx),
        }),
    }
}

/// Provision the dev account before serving (idempotent). Opens its own short
/// connection; `serve` opens the same DB file afterward.
fn ensure_dev_account(db_path: &str) -> anyhow::Result<()> {
    let conn = cartopolis_simulator::db::open_and_migrate(db_path)?;
    cartopolis_simulator::ensure_account(&conn, DEV_USER, DEV_PASSWORD)?;
    Ok(())
}
