//! `cartopolis-server` replacing itself with a newer release, when an admin says
//! so (`docs/mockups/release-approval`).
//!
//! Every six hours it fetches `release.json` and its signature from the update
//! source (cartopolis.org/server by default). It acts only on a release signed
//! by a key built into this program ([`RELEASE_KEYS`]: the owner's phone), with
//! a higher version code than its own, and downloads its package into
//! `data/update/`, checking size and sha256. The Admin page then shows it; an
//! admin's **Install** (with their password again) runs [`install`]:
//!
//! 1. unpack the package into `next/` beside this program;
//! 2. ask the new program its version code, which must be the release's;
//! 3. let the new program migrate a *copy* of the database;
//! 4. keep this program, its web UI and a snapshot of the database in
//!    `previous/`;
//! 5. move the new ones into place and end, for systemd to start the new one.
//!
//! The new program counts its own starts (`data/update/pending.json`): after
//! [`MAX_STARTS`] that did not stay up for [`HEALTHY_AFTER`], it puts the
//! previous program, web UI and database back and ends, so the old one starts.
//!
//! What it never does: install without an admin, install a release this
//! program's keys did not sign, go back to a lower version code, follow a
//! redirect, or fetch over plain http from anywhere but this machine.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cartopolis_simulator::server_update::ServerUpdates;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The keys a release must be signed with: P-256 public keys as 65-byte
/// uncompressed points in hex, the form an Android Keystore key's public half
/// ends in. Empty until the owner's phone has made its key: then nothing can be
/// installed, which is the safe way round. A build can add one more with
/// `CARTOPOLIS_RELEASE_KEY` at compile time (its own releases, or a test).
const RELEASE_KEYS: &[&str] = &[];

/// The download inside every release.
pub const PACKAGE: &str = "cartopolis-server-linux-x86_64";
const MANIFEST: &str = "release.json";
const SIGNATURE: &str = "release.json.sig";

const CHECK_EVERY: Duration = Duration::from_secs(6 * 3600);
const FIRST_CHECK_AFTER: Duration = Duration::from_secs(60);
const MAX_MANIFEST: u64 = 64 * 1024;
const MAX_SIGNATURE: u64 = 4 * 1024;
const MAX_PACKAGE: u64 = 300 * 1024 * 1024;
/// Starts of a new version that did not stay up, before it is rolled back.
pub const MAX_STARTS: u32 = 3;
/// How long a new version must run to count as installed.
pub const HEALTHY_AFTER: Duration = Duration::from_secs(120);

/// This program's version code, set by the release build (20000 + commits on
/// main, as the app's); 0 in a development build, which never updates.
pub fn build_code() -> u32 {
    option_env!("CARTOPOLIS_BUILD_CODE")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

pub fn build_name() -> &'static str {
    option_env!("CARTOPOLIS_BUILD_NAME").unwrap_or("development build")
}

/// [`RELEASE_KEYS`] and the build's own, decoded.
pub fn trusted_keys() -> Vec<Vec<u8>> {
    RELEASE_KEYS
        .iter()
        .copied()
        .chain(
            option_env!("CARTOPOLIS_RELEASE_KEY")
                .into_iter()
                .flat_map(|k| k.split(',')),
        )
        .filter_map(|k| hex_decode(k.trim()))
        .filter(|k| k.len() == 65 && k[0] == 4)
        .collect()
}

/// `[update]` in `cartopolis.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateConfig {
    /// Where releases are published; `release.json` is read from here.
    pub url: String,
    /// Look for releases at all.
    pub check: bool,
}

impl Default for UpdateConfig {
    fn default() -> Self {
        Self {
            url: "https://cartopolis.org/server".into(),
            check: true,
        }
    }
}

impl UpdateConfig {
    pub fn load(file: &Path) -> anyhow::Result<Self> {
        use figment::providers::{Format, Serialized, Toml};
        Ok(
            figment::Figment::from(Serialized::defaults(Self::default()))
                .merge(figment::Figment::from(Toml::file(file)).focus("update"))
                .extract()?,
        )
    }
}

/// What the owner signed: `release.json`, verbatim.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Release {
    pub code: u32,
    pub name: String,
    #[serde(default)]
    pub published: String,
    /// One line per change, in the words its landing was put to the owner in.
    #[serde(default)]
    pub changes: Vec<String>,
    pub server: Package,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Package {
    pub file: String,
    pub sha256: String,
    pub size: u64,
}

/// Check `manifest` against `signature` (base64 of an ASN.1 ECDSA P-256 /
/// SHA-256 signature, what Android's `SHA256withECDSA` gives) with each of
/// `keys`, then read it. Returns the release and which key signed it.
pub fn verify_release(
    manifest: &[u8],
    signature: &str,
    keys: &[Vec<u8>],
) -> Result<(Release, usize), String> {
    use base64::Engine as _;
    let sig = base64::engine::general_purpose::STANDARD
        .decode(signature.trim())
        .map_err(|_| "the signature is not base64".to_string())?;
    let signer = keys
        .iter()
        .position(|key| {
            ring::signature::UnparsedPublicKey::new(&ring::signature::ECDSA_P256_SHA256_ASN1, key)
                .verify(manifest, &sig)
                .is_ok()
        })
        .ok_or("the release is not signed with a key this server trusts")?;
    let release: Release =
        serde_json::from_slice(manifest).map_err(|e| format!("release.json does not read: {e}"))?;
    if release.server.file != format!("{PACKAGE}.tar.gz") {
        return Err(format!("unexpected package name {:?}", release.server.file));
    }
    if release.server.sha256.len() != 64 || hex_decode(&release.server.sha256).is_none() {
        return Err("the package checksum is not a sha256".into());
    }
    if release.server.size > MAX_PACKAGE {
        return Err("the package is larger than a server package can be".into());
    }
    Ok((release, signer))
}

/// A short name for a key, as the Admin page shows who signed: its first bytes
/// after the point marker.
pub fn key_label(key: &[u8]) -> String {
    hex_encode(&Sha256::digest(key)[..6])
}

/// Where the program, its web UI, the data and the update's files are.
#[derive(Debug, Clone)]
pub struct Paths {
    /// The directory this program runs from (`/opt/cartopolis-server`).
    pub program: PathBuf,
    /// The world database.
    pub db: PathBuf,
    /// `data/update/`: downloads, the pending marker, the last result.
    pub work: PathBuf,
}

impl Paths {
    pub fn new(db: &str) -> anyhow::Result<Self> {
        let exe = std::env::current_exe()?;
        let program = exe
            .parent()
            .ok_or_else(|| anyhow::anyhow!("this program has no directory"))?
            .to_path_buf();
        let db = PathBuf::from(db);
        let data = db.parent().map(Path::to_path_buf).unwrap_or_default();
        Ok(Self {
            program,
            work: data.join("update"),
            db,
        })
    }
    fn previous(&self) -> PathBuf {
        self.program.join("previous")
    }
    fn next(&self) -> PathBuf {
        self.program.join("next")
    }
    fn pending(&self) -> PathBuf {
        self.work.join("pending.json")
    }
    fn last(&self) -> PathBuf {
        self.work.join("last.json")
    }
    fn failed(&self) -> PathBuf {
        self.work.join("failed.json")
    }
    fn download(&self, code: u32) -> PathBuf {
        self.work.join(format!("{code}.tar.gz"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
enum Phase {
    Idle,
    Checking,
    Downloading,
    Ready,
    Installing,
    Restarting,
    Failed,
}

#[derive(Debug)]
struct State {
    phase: Phase,
    available: Option<Release>,
    signed_by: Option<String>,
    last_check: Option<u64>,
    message: Option<String>,
    steps: Vec<String>,
}

/// Started once per marker, after a version stayed up or was given up on.
#[derive(Debug, Serialize, Deserialize)]
struct Pending {
    code: u32,
    from: u32,
    by: String,
    at: u64,
    starts: u32,
}

pub struct Updater {
    config: UpdateConfig,
    paths: Paths,
    keys: Vec<Vec<u8>>,
    state: Mutex<State>,
    wake: Condvar,
    /// Told when the new version is in place, for `main` to end and systemd to
    /// start it.
    restart: Arc<tokio::sync::Notify>,
}

impl Updater {
    pub fn start(
        config: UpdateConfig,
        paths: Paths,
        restart: Arc<tokio::sync::Notify>,
    ) -> Arc<Self> {
        let keys = trusted_keys();
        let message = if build_code() == 0 {
            Some("a development build does not update itself".to_string())
        } else if keys.is_empty() {
            Some("this build trusts no release key yet, so it installs no update".to_string())
        } else if !config.check {
            Some("looking for updates is off ([update] check = false)".to_string())
        } else {
            None
        };
        let active = message.is_none();
        let updater = Arc::new(Self {
            config,
            paths,
            keys,
            state: Mutex::new(State {
                phase: Phase::Idle,
                available: None,
                signed_by: None,
                last_check: None,
                message,
                steps: Vec::new(),
            }),
            wake: Condvar::new(),
            restart,
        });
        if active {
            let me = updater.clone();
            std::thread::Builder::new()
                .name("updates".into())
                .spawn(move || me.run())
                .expect("start the update thread");
        }
        updater
    }

    fn run(self: Arc<Self>) {
        let mut wait = FIRST_CHECK_AFTER;
        loop {
            {
                let state = self.state.lock().unwrap();
                let _ = self.wake.wait_timeout(state, wait).unwrap();
            }
            wait = CHECK_EVERY;
            let busy = matches!(
                self.state.lock().unwrap().phase,
                Phase::Installing | Phase::Restarting
            );
            if !busy {
                self.check();
            }
        }
    }

    fn set(&self, f: impl FnOnce(&mut State)) {
        f(&mut self.state.lock().unwrap());
    }

    fn check(&self) {
        self.set(|s| s.phase = Phase::Checking);
        let result = self.check_inner();
        self.set(|s| {
            s.last_check = Some(unix_now());
            match result {
                Ok(()) => {}
                Err(e) => {
                    tracing::warn!("server update: {e}");
                    s.phase = Phase::Idle;
                    s.message = Some(e);
                }
            }
        });
    }

    fn check_inner(&self) -> Result<(), String> {
        let base = self.config.url.trim_end_matches('/');
        allowed_url(base)?;
        let manifest = fetch(&format!("{base}/{MANIFEST}"), MAX_MANIFEST)?;
        let signature = fetch(&format!("{base}/{SIGNATURE}"), MAX_SIGNATURE).map_err(|e| {
            if e.contains("status code 404") {
                "the newest release is not signed yet, so there is nothing to install".to_string()
            } else {
                e
            }
        })?;
        let signature = String::from_utf8(signature).map_err(|_| "the signature is not text")?;
        let (release, signer) = verify_release(&manifest, &signature, &self.keys)?;
        let signed_by = key_label(&self.keys[signer]);
        if release.code <= build_code() {
            self.set(|s| {
                s.phase = Phase::Idle;
                s.available = None;
                s.message = None;
            });
            return Ok(());
        }
        if read_json::<Release>(&self.paths.failed()).is_some_and(|f| f.code == release.code) {
            self.set(|s| {
                s.phase = Phase::Idle;
                s.available = None;
                s.message = Some(format!(
                    "release {} did not start here and was rolled back; waiting for a newer one",
                    release.name
                ));
            });
            return Ok(());
        }
        let file = self.paths.download(release.code);
        if sha256_file(&file).ok().as_deref() != Some(release.server.sha256.as_str()) {
            self.set(|s| s.phase = Phase::Downloading);
            std::fs::create_dir_all(&self.paths.work).map_err(|e| e.to_string())?;
            download(
                &format!("{base}/{}", release.server.file),
                &file,
                &release.server,
            )?;
        }
        tracing::info!(code = release.code, name = %release.name, key = %signed_by, "server update ready to install");
        self.set(|s| {
            s.phase = Phase::Ready;
            s.available = Some(release);
            s.signed_by = Some(signed_by);
            s.message = None;
        });
        Ok(())
    }

    fn step(&self, line: impl Into<String>) {
        let line = line.into();
        tracing::info!("server update: {line}");
        self.set(|s| s.steps.push(line));
    }

    fn install_inner(&self, release: &Release, by: &str) -> Result<(), String> {
        let file = self.paths.download(release.code);
        let sum = sha256_file(&file).map_err(|e| format!("the download is gone: {e}"))?;
        if sum != release.server.sha256 {
            return Err("the download no longer matches the signed checksum".into());
        }
        self.step(format!(
            "Package checked against the release signed with key {}",
            self.state
                .lock()
                .unwrap()
                .signed_by
                .clone()
                .unwrap_or_default()
        ));

        let next = self.paths.next();
        let _ = std::fs::remove_dir_all(&next);
        std::fs::create_dir_all(&next).map_err(|e| {
            format!(
                "cannot write beside the program in {} ({e}); it must belong to the user the server runs as",
                self.paths.program.display()
            )
        })?;
        unpack(&file, &next)?;
        let new_dir = next.join(PACKAGE);
        let new_exe = new_dir.join("cartopolis-server");
        self.step("Unpacked");

        let out = Command::new(&new_exe)
            .arg("version")
            .output()
            .map_err(|e| format!("the new program does not run: {e}"))?;
        let said = String::from_utf8_lossy(&out.stdout);
        let said_code = said
            .split_whitespace()
            .next()
            .and_then(|c| c.parse::<u32>().ok());
        if !out.status.success() {
            return Err(format!(
                "the new program does not run on this machine ({})",
                out.status
            ));
        }
        if said_code != Some(release.code) {
            return Err(format!(
                "the new program says it is {:?}, not {}",
                said.trim(),
                release.code
            ));
        }
        self.step(format!(
            "The new program runs and is version {}",
            release.code
        ));

        // In a directory of its own: opening a database also makes `stats.db`
        // beside it.
        let test = self.paths.work.join("test");
        let _ = std::fs::remove_dir_all(&test);
        std::fs::create_dir_all(&test).map_err(|e| e.to_string())?;
        let copy = test.join("world.db");
        snapshot_db(&self.paths.db, &copy)?;
        let out = Command::new(&new_exe)
            .arg("check-database")
            .arg(&copy)
            .output()
            .map_err(|e| format!("the new program does not run: {e}"))?;
        let _ = std::fs::remove_dir_all(&test);
        if !out.status.success() {
            return Err(format!(
                "the new program could not open a copy of the database: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        self.step("The new program opened and updated a copy of the database");

        let previous = self.paths.previous();
        let _ = std::fs::remove_dir_all(&previous);
        std::fs::create_dir_all(&previous).map_err(|e| e.to_string())?;
        let exe = self.paths.program.join("cartopolis-server");
        std::fs::copy(&exe, previous.join("cartopolis-server")).map_err(|e| e.to_string())?;
        let web = self.paths.program.join("web");
        if web.is_dir() {
            copy_dir(&web, &previous.join("web")).map_err(|e| e.to_string())?;
        }
        snapshot_db(&self.paths.db, &previous.join("world.db"))?;
        write_json(
            &previous.join("version.json"),
            &serde_json::json!({ "code": build_code(), "name": build_name() }),
        )?;
        self.step(format!(
            "Version {} and its database kept in {}",
            build_name(),
            previous.display()
        ));

        write_json(
            &self.paths.pending(),
            &Pending {
                code: release.code,
                from: build_code(),
                by: by.to_string(),
                at: unix_now(),
                starts: 0,
            },
        )?;
        std::fs::rename(&new_exe, &exe).map_err(|e| e.to_string())?;
        if new_dir.join("web").is_dir() {
            let _ = std::fs::remove_dir_all(&web);
            std::fs::rename(new_dir.join("web"), &web).map_err(|e| e.to_string())?;
        }
        let _ = std::fs::remove_dir_all(&next);
        let _ = std::fs::remove_file(&file);
        self.step("Switched over; restarting");
        Ok(())
    }
}

/// The updater as the admin API sees it.
pub struct Handle(pub Arc<Updater>);

impl ServerUpdates for Handle {
    fn status(&self) -> serde_json::Value {
        let me = &self.0;
        let s = me.state.lock().unwrap();
        serde_json::json!({
            "installed": { "code": build_code(), "name": build_name() },
            "source": me.config.url,
            "trusted_keys": me.keys.iter().map(|k| key_label(k)).collect::<Vec<_>>(),
            "phase": s.phase,
            "available": s.available,
            "signed_by": s.signed_by,
            "last_check": s.last_check,
            "message": s.message,
            "steps": s.steps,
            "last_result": read_json::<serde_json::Value>(&me.paths.last()),
            "previous": read_json::<serde_json::Value>(&me.paths.previous().join("version.json")),
        })
    }

    fn check_now(&self) {
        self.0.wake.notify_all();
    }

    fn install(&self, by: &str) -> Result<(), String> {
        let release = {
            let mut s = self.0.state.lock().unwrap();
            if s.phase != Phase::Ready {
                return Err("there is no verified release ready to install".into());
            }
            s.phase = Phase::Installing;
            s.steps.clear();
            s.available.clone().ok_or("there is no release")?
        };
        let me = self.0.clone();
        let by = by.to_string();
        std::thread::Builder::new()
            .name("update-install".into())
            .spawn(move || match me.install_inner(&release, &by) {
                Ok(()) => {
                    me.set(|s| s.phase = Phase::Restarting);
                    me.restart.notify_one();
                }
                Err(e) => {
                    tracing::error!("server update failed: {e}");
                    let _ = std::fs::remove_dir_all(me.paths.next());
                    me.step(format!("Stopped: {e}. Nothing was changed."));
                    me.set(|s| {
                        s.phase = Phase::Failed;
                        s.message = Some(e);
                    });
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}

/// Before the world opens: a new version counts its start and, past
/// [`MAX_STARTS`], puts the previous one back and ends so it starts instead.
/// Returns whether this is a new version on probation, for [`confirm_later`].
pub fn on_start(paths: &Paths) -> anyhow::Result<bool> {
    let Some(mut pending) = read_json::<Pending>(&paths.pending()) else {
        return Ok(false);
    };
    if pending.code != build_code() {
        // The previous version is running again: by hand, or rolled back.
        let _ = std::fs::remove_file(paths.pending());
        return Ok(false);
    }
    pending.starts += 1;
    if pending.starts <= MAX_STARTS {
        write_json(&paths.pending(), &pending).map_err(anyhow::Error::msg)?;
        tracing::info!(
            code = pending.code,
            start = pending.starts,
            "new version starting"
        );
        return Ok(true);
    }
    tracing::error!(
        code = pending.code,
        "the new version did not stay up after {MAX_STARTS} starts; putting the previous one back"
    );
    let previous = paths.previous();
    std::fs::copy(
        previous.join("cartopolis-server"),
        paths.program.join("cartopolis-server.back"),
    )?;
    std::fs::rename(
        paths.program.join("cartopolis-server.back"),
        paths.program.join("cartopolis-server"),
    )?;
    if previous.join("web").is_dir() {
        let web = paths.program.join("web");
        let _ = std::fs::remove_dir_all(&web);
        copy_dir(&previous.join("web"), &web)?;
    }
    for suffix in ["-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", paths.db.display()));
    }
    std::fs::copy(previous.join("world.db"), &paths.db)?;
    write_json(
        &paths.failed(),
        &serde_json::json!({ "code": pending.code }),
    )
    .map_err(anyhow::Error::msg)?;
    write_json(
        &paths.last(),
        &serde_json::json!({
            "result": "rolled back",
            "code": pending.code,
            "back_to": pending.from,
            "by": pending.by,
            "at": unix_now(),
        }),
    )
    .map_err(anyhow::Error::msg)?;
    let _ = std::fs::remove_file(paths.pending());
    anyhow::bail!("rolled back to version {}; starting it", pending.from)
}

/// A new version that stays up [`HEALTHY_AFTER`] is installed: say so, and
/// stop counting its starts.
pub fn confirm_later(paths: Paths) {
    std::thread::spawn(move || {
        std::thread::sleep(HEALTHY_AFTER);
        let Some(pending) = read_json::<Pending>(&paths.pending()) else {
            return;
        };
        let _ = write_json(
            &paths.last(),
            &serde_json::json!({
                "result": "installed",
                "code": pending.code,
                "from": pending.from,
                "by": pending.by,
                "at": unix_now(),
            }),
        );
        let _ = std::fs::remove_file(paths.pending());
        tracing::info!(code = pending.code, "new version installed");
    });
}

/// `https`, or plain http to this machine only (a test).
fn allowed_url(url: &str) -> Result<(), String> {
    if url.starts_with("https://") {
        return Ok(());
    }
    if let Some(rest) = url.strip_prefix("http://") {
        let host = rest.split(['/', ':']).next().unwrap_or("");
        if host == "127.0.0.1" || host == "localhost" || rest.starts_with("[::1]") {
            return Ok(());
        }
    }
    Err(format!("updates come over https only, not from {url}"))
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .redirects(0)
        .timeout_connect(Duration::from_secs(15))
        .timeout_read(Duration::from_secs(60))
        .user_agent(&format!("cartopolis-server/{}", build_code()))
        .build()
}

fn fetch(url: &str, max: u64) -> Result<Vec<u8>, String> {
    let response = agent()
        .get(url)
        .call()
        .map_err(|e| format!("could not fetch {url}: {e}"))?;
    if response.status() != 200 {
        return Err(format!("{url} answered {}", response.status()));
    }
    let mut body = Vec::new();
    response
        .into_reader()
        .take(max + 1)
        .read_to_end(&mut body)
        .map_err(|e| format!("could not read {url}: {e}"))?;
    if body.len() as u64 > max {
        return Err(format!("{url} is larger than it may be"));
    }
    Ok(body)
}

fn download(url: &str, to: &Path, package: &Package) -> Result<(), String> {
    let response = agent()
        .get(url)
        .call()
        .map_err(|e| format!("could not fetch {url}: {e}"))?;
    if response.status() != 200 {
        return Err(format!("{url} answered {}", response.status()));
    }
    let part = to.with_extension("part");
    let mut file = std::fs::File::create(&part).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut reader = response.into_reader().take(package.size + 1);
    let mut buf = vec![0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let n = reader.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        total += n as u64;
        hasher.update(&buf[..n]);
        std::io::Write::write_all(&mut file, &buf[..n]).map_err(|e| e.to_string())?;
    }
    let sum = hex_encode(&hasher.finalize());
    if total != package.size || sum != package.sha256 {
        let _ = std::fs::remove_file(&part);
        return Err("the download does not match the signed size and checksum".into());
    }
    std::fs::rename(&part, to).map_err(|e| e.to_string())
}

/// Unpack only what a package holds — the program and its web UI — refusing
/// any path that would land outside `into`.
fn unpack(file: &Path, into: &Path) -> Result<(), String> {
    let f = std::fs::File::open(file).map_err(|e| e.to_string())?;
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(f));
    for entry in archive.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path().map_err(|e| e.to_string())?.into_owned();
        let kind = entry.header().entry_type();
        if !(kind.is_file() || kind.is_dir()) {
            return Err(format!(
                "the package holds a link or device: {}",
                path.display()
            ));
        }
        if !path.starts_with(PACKAGE) {
            return Err(format!(
                "the package holds {} outside {PACKAGE}/",
                path.display()
            ));
        }
        if !entry.unpack_in(into).map_err(|e| e.to_string())? {
            return Err(format!(
                "the package tried to write outside: {}",
                path.display()
            ));
        }
    }
    Ok(())
}

/// A consistent copy of the live database, taken through SQLite while the
/// server keeps writing to it.
fn snapshot_db(db: &Path, to: &Path) -> Result<(), String> {
    let conn =
        rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| format!("cannot open the database: {e}"))?;
    conn.execute("VACUUM INTO ?1", [to.display().to_string()])
        .map_err(|e| format!("cannot copy the database: {e}"))?;
    Ok(())
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)?;
    Ok(hex_encode(&hasher.finalize()))
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(
        &tmp,
        serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::rand::SystemRandom;
    use ring::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_ASN1_SIGNING};

    fn key() -> EcdsaKeyPair {
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng).unwrap();
        EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8.as_ref(), &rng).unwrap()
    }

    fn sign(key: &EcdsaKeyPair, bytes: &[u8]) -> String {
        use base64::Engine as _;
        let sig = key.sign(&SystemRandom::new(), bytes).unwrap();
        base64::engine::general_purpose::STANDARD.encode(sig.as_ref())
    }

    fn manifest() -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "code": 22140,
            "name": "22140",
            "changes": ["One line"],
            "server": {
                "file": format!("{PACKAGE}.tar.gz"),
                "sha256": "0".repeat(64),
                "size": 1000,
            }
        }))
        .unwrap()
    }

    #[test]
    fn a_release_signed_by_a_trusted_key_reads() {
        let k = key();
        let keys = vec![
            key().public_key().as_ref().to_vec(),
            k.public_key().as_ref().to_vec(),
        ];
        let m = manifest();
        let (release, signer) = verify_release(&m, &sign(&k, &m), &keys).unwrap();
        assert_eq!((release.code, signer), (22140, 1));
    }

    #[test]
    fn a_release_signed_by_another_key_or_changed_after_signing_is_refused() {
        let (trusted, other) = (key(), key());
        let keys = vec![trusted.public_key().as_ref().to_vec()];
        let m = manifest();
        assert!(verify_release(&m, &sign(&other, &m), &keys).is_err());
        let sig = sign(&trusted, &m);
        let mut changed = m.clone();
        let at = changed.iter().position(|&b| b == b'2').unwrap();
        changed[at] = b'3';
        assert!(verify_release(&changed, &sig, &keys).is_err());
        assert!(verify_release(&m, &sig, &[]).is_err());
    }

    #[test]
    fn only_https_or_this_machine() {
        assert!(allowed_url("https://cartopolis.org/server").is_ok());
        assert!(allowed_url("http://127.0.0.1:8000").is_ok());
        assert!(allowed_url("http://cartopolis.org/server").is_err());
        assert!(allowed_url("http://127.0.0.1.evil.org/x").is_err());
        assert!(allowed_url("file:///etc/passwd").is_err());
    }

    #[test]
    fn a_new_version_that_keeps_failing_is_rolled_back() {
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("opt");
        let paths = Paths {
            program: program.clone(),
            db: dir.path().join("data/world.db"),
            work: dir.path().join("data/update"),
        };
        std::fs::create_dir_all(program.join("previous/web")).unwrap();
        std::fs::create_dir_all(dir.path().join("data")).unwrap();
        std::fs::write(program.join("previous/cartopolis-server"), b"old").unwrap();
        std::fs::write(program.join("previous/web/index.html"), b"old page").unwrap();
        std::fs::write(program.join("previous/world.db"), b"old db").unwrap();
        std::fs::write(program.join("cartopolis-server"), b"new").unwrap();
        std::fs::write(&paths.db, b"new db").unwrap();
        let pending = Pending {
            code: build_code(),
            from: 1,
            by: "anna".into(),
            at: 0,
            starts: 0,
        };
        write_json(&paths.pending(), &pending).unwrap();
        for _ in 0..MAX_STARTS {
            assert!(on_start(&paths).unwrap());
        }
        assert!(on_start(&paths).is_err());
        assert_eq!(
            std::fs::read(program.join("cartopolis-server")).unwrap(),
            b"old"
        );
        assert_eq!(
            std::fs::read(program.join("web/index.html")).unwrap(),
            b"old page"
        );
        assert_eq!(std::fs::read(&paths.db).unwrap(), b"old db");
        assert!(read_json::<Pending>(&paths.pending()).is_none());
        assert_eq!(
            read_json::<serde_json::Value>(&paths.failed()).unwrap()["code"],
            build_code()
        );
    }
}
