//! A release approved on the owner's phone (`docs/mockups/release-approval`).
//!
//! A session asks through the Midas link (`approve_release`, with where the
//! release is published). This phone then fetches `release.json` and every
//! package it names **itself**, checks each against the size and sha256 the
//! release states, and shows the owner what changes and what was checked. Only
//! on their tap and their fingerprint does `platform::release_key` sign the
//! exact bytes of `release.json`; the signature goes back to the session that
//! asked, which publishes it as `release.json.sig`. A session can ask and can
//! tap the button, but not give the fingerprint.
//!
//! Developer tools ▸ Midas also makes the key ([`key_section`]), and the link's
//! `release_key` tool tells a session its public half, which goes into the
//! server's and the app's `RELEASE_KEYS`.

use std::io::Read;
use std::sync::Mutex;

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::platform::release_key::{self, Signing};
use crate::systems::egui_manager::EguiRef;
use crate::systems::theme;

/// Largest package this phone downloads to check: the APK is about 80 MB.
const MAX_PACKAGE: u64 = 300 * 1024 * 1024;
const MAX_MANIFEST: u64 = 64 * 1024;
const CARD_WIDTH: f32 = 420.0;

/// What a release says it is: the server's `release.json`, with the APK
/// beside it when the release carries one.
#[derive(Clone, Debug, Deserialize)]
struct Release {
    code: u32,
    name: String,
    #[serde(default)]
    changes: Vec<String>,
    server: Option<Package>,
    apk: Option<Package>,
}

#[derive(Clone, Debug, Deserialize)]
struct Package {
    file: String,
    sha256: String,
    size: u64,
}

#[derive(Clone, Debug)]
struct Check {
    ok: bool,
    line: String,
}

#[derive(Clone, Debug, PartialEq)]
enum Phase {
    /// Fetching and hashing; what it is at.
    Checking(String),
    Ready,
    Signing,
}

#[derive(Clone, Debug)]
struct Request {
    session: String,
    source: String,
    manifest: Vec<u8>,
    release: Option<Release>,
    checks: Vec<Check>,
    phase: Phase,
}

#[derive(Default)]
struct State {
    request: Option<Request>,
    /// For the session that asked: (session, words).
    outcome: Option<(String, String)>,
    /// Developer tools ▸ Midas ▸ Release key, read once.
    key: Option<KeyView>,
}

#[derive(Clone, Default)]
struct KeyView {
    public: Option<String>,
    location: Option<String>,
    message: Option<String>,
}

static STATE: Mutex<State> = Mutex::new(State {
    request: None,
    outcome: None,
    key: None,
});

fn state() -> std::sync::MutexGuard<'static, State> {
    STATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The link's `approve_release`: fetch and check the release at `source` and
/// put it in front of the owner. Refused while another is open.
pub fn request(session: &str, source: &str) -> Result<(), String> {
    if !release_key::available() {
        return Err("releases are approved on the owner's Android phone".into());
    }
    if release_key::public_key()?.is_none() {
        return Err(
            "this phone has no release key yet: Developer tools ▸ Midas ▸ Make release key".into(),
        );
    }
    let source = source.trim_end_matches('/').to_string();
    if !source.starts_with("https://") {
        return Err("a release is fetched over https only".into());
    }
    {
        let mut s = state();
        if s.request.is_some() {
            return Err("another release is waiting for the owner already".into());
        }
        s.request = Some(Request {
            session: session.to_string(),
            source: source.clone(),
            manifest: Vec::new(),
            release: None,
            checks: Vec::new(),
            phase: Phase::Checking("Fetching release.json".into()),
        });
    }
    std::thread::Builder::new()
        .name("release-check".into())
        .spawn(move || check(&source))
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// The words for the session that asked, once the owner has answered.
pub fn take_outcome() -> Option<(String, String)> {
    state().outcome.take()
}

/// The link's `release_key`: the public key and where it lives.
pub fn key_report() -> String {
    match release_key::public_key() {
        Ok(Some(k)) => format!(
            "release key {k}\nkept in {}: P-256, signs only after a strong biometric",
            release_key::location().unwrap_or_else(|| "an unknown place".into())
        ),
        Ok(None) => {
            "This phone has no release key yet: Developer tools ▸ Midas ▸ Make release key.".into()
        }
        Err(e) => format!("could not read the release key: {e}"),
    }
}

fn progress(line: impl Into<String>) {
    if let Some(r) = state().request.as_mut() {
        r.phase = Phase::Checking(line.into());
    }
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .redirects(0)
        .timeout_connect(std::time::Duration::from_secs(15))
        .timeout_read(std::time::Duration::from_secs(60))
        .build()
}

/// Fetch `release.json` and hash every package it names, on this phone.
fn check(source: &str) {
    let mut checks = Vec::new();
    let manifest = match fetch(&format!("{source}/release.json")) {
        Ok(m) => m,
        Err(e) => return finish(Vec::new(), None, vec![Check { ok: false, line: e }]),
    };
    let release: Release = match serde_json::from_slice(&manifest) {
        Ok(r) => r,
        Err(e) => {
            return finish(
                manifest,
                None,
                vec![Check {
                    ok: false,
                    line: format!("release.json does not read: {e}"),
                }],
            )
        }
    };
    let packages: Vec<(&str, &Package)> = [("server", &release.server), ("app", &release.apk)]
        .into_iter()
        .filter_map(|(what, p)| p.as_ref().map(|p| (what, p)))
        .collect();
    if packages.is_empty() {
        checks.push(Check {
            ok: false,
            line: "the release names no package".into(),
        });
    }
    for (what, package) in packages {
        progress(format!(
            "Downloading the {what} ({} MB) to check it",
            package.size / (1024 * 1024)
        ));
        checks.push(match hash(&format!("{source}/{}", package.file), package) {
            Ok(()) => Check {
                ok: true,
                line: format!(
                    "This phone downloaded the {what} and checked it: {} MB, sha256 {}…",
                    package.size / (1024 * 1024),
                    &package.sha256[..12.min(package.sha256.len())]
                ),
            },
            Err(e) => Check {
                ok: false,
                line: format!("The {what}: {e}"),
            },
        });
    }
    finish(manifest, Some(release), checks);
}

fn finish(manifest: Vec<u8>, release: Option<Release>, checks: Vec<Check>) {
    if let Some(r) = state().request.as_mut() {
        r.manifest = manifest;
        r.release = release;
        r.checks = checks;
        r.phase = Phase::Ready;
    }
}

fn fetch(url: &str) -> Result<Vec<u8>, String> {
    let response = agent()
        .get(url)
        .call()
        .map_err(|e| format!("could not fetch {url}: {e}"))?;
    let mut body = Vec::new();
    response
        .into_reader()
        .take(MAX_MANIFEST + 1)
        .read_to_end(&mut body)
        .map_err(|e| e.to_string())?;
    if body.len() as u64 > MAX_MANIFEST {
        return Err(format!("{url} is larger than a release.json can be"));
    }
    Ok(body)
}

/// Download `url` without keeping it, and compare its size and sha256.
fn hash(url: &str, package: &Package) -> Result<(), String> {
    if package.size > MAX_PACKAGE {
        return Err("larger than a package can be".into());
    }
    let response = agent()
        .get(url)
        .call()
        .map_err(|e| format!("could not fetch it: {e}"))?;
    let mut reader = response.into_reader().take(package.size + 1);
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let n = reader.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        total += n as u64;
        hasher.update(&buf[..n]);
    }
    let sum: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if total != package.size {
        return Err(format!(
            "{total} bytes, not the {} the release says",
            package.size
        ));
    }
    if sum != package.sha256 {
        return Err("its sha256 is not the one the release says".into());
    }
    Ok(())
}

/// Developer tools ▸ Midas ▸ Release key.
pub fn key_section(ui: &mut egui::Ui) {
    if !release_key::available() {
        return;
    }
    ui.separator();
    theme::section(ui, "Release key");
    let view = {
        let mut s = state();
        s.key
            .get_or_insert_with(|| KeyView {
                public: release_key::public_key().ok().flatten(),
                location: release_key::location(),
                message: None,
            })
            .clone()
    };
    match &view.public {
        None => {
            theme::note(ui, "Approves releases of the app and of self-hosted servers with your fingerprint. Made once, on this phone; it never leaves its security chip.");
            if theme::primary_button_sized(ui, "Make release key", 200.0).clicked() {
                let made = release_key::create();
                let mut s = state();
                s.key = Some(match made {
                    Ok(k) => KeyView {
                        public: Some(k),
                        location: release_key::location(),
                        message: None,
                    },
                    Err(e) => KeyView {
                        message: Some(format!("Could not make it: {e}")),
                        ..KeyView::default()
                    },
                });
            }
        }
        Some(key) => {
            theme::note(
                ui,
                &format!(
                    "Kept in {}; signs only after your fingerprint.",
                    match view.location.as_deref() {
                        Some("strongbox") => "the phone's separate security chip (StrongBox)",
                        Some("tee") => "the phone's secure area (TEE)",
                        Some("software") => "software only — do not sign releases with it",
                        _ => "the Android Keystore",
                    }
                ),
            );
            ui.label(
                egui::RichText::new(format!("{}…{}", &key[..16], &key[key.len() - 8..]))
                    .monospace(),
            );
            if ui.button("Copy public key").clicked() {
                ui.ctx().copy_text(key.clone());
            }
            theme::note(ui, "A session reads it with the link's release_key tool and builds it into the server and the app as the key they trust.");
        }
    }
    if let Some(m) = &view.message {
        let _ = theme::inline_error(ui, m);
    }
}

/// The release in front of the owner, over everything; and the fingerprint
/// sheet's outcome once it is answered.
pub fn render_release_approval(egui: EguiRef) {
    let Some(request) = state().request.clone() else {
        return;
    };
    if request.phase == Phase::Signing {
        match release_key::status() {
            Signing::Asking | Signing::Idle => {}
            Signing::Signed(signature) => {
                let name = request
                    .release
                    .as_ref()
                    .map_or_else(String::new, |r| r.name.clone());
                let key = release_key::public_key().ok().flatten().unwrap_or_default();
                answer(
                    &request,
                    format!(
                        "Release {name} approved on the owner's phone with their fingerprint.\n\
                         release.json.sig for {}/release.json:\n{signature}\n\
                         signed with key {key}",
                        request.source
                    ),
                );
                return;
            }
            Signing::Cancelled => {
                set_phase(Phase::Ready);
            }
            Signing::Failed(why) => {
                set_phase(Phase::Ready);
                let mut s = state();
                if let Some(r) = s.request.as_mut() {
                    r.checks.push(Check {
                        ok: false,
                        line: format!("Signing did not work: {why}"),
                    });
                }
            }
        }
        egui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(300));
    }
    let ctx = egui.ctx();
    let screen = ctx.content_rect();
    let width = CARD_WIDTH.min(screen.width() - 2.0 * theme::OSD_MARGIN);
    egui::Area::new(egui::Id::new("release_approval_veil"))
        .order(egui::Order::Foreground)
        .fixed_pos(screen.min)
        .show(ctx, |ui| {
            ui.painter()
                .rect_filled(screen, 0.0, theme::DARK.ink.gamma_multiply(0.86));
            // unnamed: a blocker under the card, not a control
            ui.allocate_rect(screen, egui::Sense::click_and_drag());
        });
    let mut approve = false;
    let mut decline = false;
    egui::Area::new(egui::Id::new("release_approval_card"))
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            theme::overlay_frame().show(ui, |ui| {
                ui.set_width(width);
                let title = request.release.as_ref().map_or_else(
                    || "A release for your approval".to_string(),
                    |r| format!("Release {}", r.name),
                );
                ui.label(
                    egui::RichText::new(title)
                        .size(theme::SIZE_HEADING)
                        .color(theme::p().cream),
                );
                line(
                    ui,
                    &format!("Asked by {} · {}", request.session, request.source),
                );
                egui::ScrollArea::vertical()
                    .max_height(screen.height() * 0.55)
                    .show(ui, |ui| {
                        if let Some(r) = &request.release {
                            theme::section(ui, &format!("What changes · {}", r.changes.len()));
                            if r.changes.is_empty() {
                                line(ui, "The release lists no changes.");
                            }
                            for c in &r.changes {
                                ui.label(format!("• {c}"));
                            }
                            line(ui, &format!("Version code {}", r.code));
                        }
                        theme::section(ui, "Checks");
                        if let Phase::Checking(what) = &request.phase {
                            ui.horizontal(|ui| {
                                ui.add(egui::Spinner::new().color(theme::p().accent));
                                line(ui, what);
                            });
                        }
                        for c in &request.checks {
                            let mark = if c.ok {
                                crate::systems::icons::CHECK
                            } else {
                                "!"
                            };
                            let colour = if c.ok {
                                theme::p().accent
                            } else {
                                theme::p().error
                            };
                            ui.horizontal_wrapped(|ui| {
                                ui.label(egui::RichText::new(mark).color(colour));
                                ui.label(&c.line);
                            });
                        }
                    });
                ui.add_space(8.0);
                let ready = request.phase == Phase::Ready
                    && request.release.is_some()
                    && !request.checks.is_empty()
                    && request.checks.iter().all(|c| c.ok);
                let label = if request.phase == Phase::Signing {
                    "Waiting for your fingerprint…"
                } else {
                    "Approve with fingerprint"
                };
                let width = ui.available_width();
                if ui
                    .add_enabled_ui(ready, |ui| theme::primary_button_sized(ui, label, width))
                    .inner
                    .clicked()
                {
                    approve = true;
                }
                if request.phase == Phase::Ready && !ready && !request.checks.is_empty() {
                    line(ui, "A check failed, so there is nothing to approve.");
                }
                if ui.button("Not now").clicked() {
                    decline = true;
                }
            });
        });
    if approve {
        let name = request
            .release
            .as_ref()
            .map_or_else(String::new, |r| r.name.clone());
        match release_key::sign(
            &request.manifest,
            &format!("Approve Cartopolis release {name}"),
            "Publishing it lets every server and phone that trusts your key install it.",
        ) {
            Ok(()) => set_phase(Phase::Signing),
            Err(e) => {
                let mut s = state();
                if let Some(r) = s.request.as_mut() {
                    r.checks.push(Check {
                        ok: false,
                        line: format!("Signing did not start: {e}"),
                    });
                }
            }
        }
    }
    if decline {
        let name = request
            .release
            .as_ref()
            .map_or_else(|| request.source.clone(), |r| r.name.clone());
        answer(
            &request,
            format!("The owner did not approve release {name}."),
        );
    }
}

fn set_phase(phase: Phase) {
    if let Some(r) = state().request.as_mut() {
        r.phase = phase;
    }
}

/// Close the request and leave the words for the session that asked.
fn answer(request: &Request, words: String) {
    let mut s = state();
    s.request = None;
    s.outcome = Some((request.session.clone(), words));
}

fn line(ui: &mut egui::Ui, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .size(theme::SIZE_SUB)
            .color(theme::p().text_dim),
    );
}
