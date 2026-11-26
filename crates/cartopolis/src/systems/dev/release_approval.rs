//! A release approved on the owner's phone (`docs/mockups/release-approval`).
//!
//! A session asks through the Midas link (`approve_release`, with where the
//! release is published). This phone then fetches `release.json` and every
//! package it names **itself**, checks each against the size and sha256 the
//! release states, and has Claude review the changes since the last release
//! it approved (`release_review`, straight to the Claude API with the owner's
//! key). The owner reads nothing: Claude's verdict is at the top, and only a
//! green one with every check passed offers **Approve with fingerprint**. Only
//! then does `platform::release_key` sign the exact bytes of `release.json`;
//! the signature goes back to the session that asked, which publishes it as
//! `release.json.sig`. A session can ask and can tap the button, but not give
//! the fingerprint.
//!
//! Developer tools ▸ Midas also makes the key and keeps the Claude API key
//! ([`key_view`], [`key_act`]), and the link's `release_key` tool tells a session the
//! key's public half, which goes into the server's and the app's
//! `RELEASE_KEYS`.

use std::io::Read;
use std::sync::Mutex;

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::platform::release_key::{self, Signing};
use crate::systems::release_review::{self, Verdict};

/// Where the code of a release is read from, by commit id: a commit id names
/// its content, so this host can refuse but not substitute.
const REPOSITORY: &str = "https://code.garage44.eu/jeroen/cartopolis";

/// Largest package this phone downloads to check: the APK is about 80 MB.
const MAX_PACKAGE: u64 = 300 * 1024 * 1024;
const MAX_MANIFEST: u64 = 64 * 1024;
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub(crate) const CARD_WIDTH: f32 = 420.0;

/// What a release says it is: the server's `release.json`, with the APK
/// beside it when the release carries one.
#[derive(Clone, Debug, Deserialize)]
struct Release {
    code: u32,
    name: String,
    #[serde(default)]
    changes: Vec<String>,
    /// The commit it was built from: where Claude's review ends.
    #[serde(default)]
    commit: Option<String>,
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

/// Claude's last check.
#[derive(Clone, Debug, PartialEq)]
enum Review {
    /// Not yet asked, or being asked; what it is at.
    Pending(String),
    Done(Verdict),
    /// No verdict: why. Red, never green.
    Failed(String),
}

#[derive(Clone, Debug)]
struct Request {
    session: String,
    source: String,
    /// The commit the review starts from: the last one this phone approved.
    since: Option<String>,
    manifest: Vec<u8>,
    release: Option<Release>,
    checks: Vec<Check>,
    review: Review,
    phase: Phase,
}

#[derive(Default)]
struct State {
    request: Option<Request>,
    /// For the session that asked: (session, words).
    outcome: Option<(String, String)>,
    /// Developer tools ▸ Midas ▸ Release key, read once.
    key: Option<KeyView>,
    /// The card as the library draws it (`lib_release_approval`), published
    /// each frame by [`drive_release_approval`].
    lib_view: Option<ApprovalView>,
    /// The library's taps, done next frame by [`act`].
    lib_acts: Vec<ApprovalAct>,
}

/// A tap on the card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub enum ApprovalAct {
    Approve,
    Decline,
}

/// Claude's verdict as the card words it ([`verdict_view`]).
#[derive(Clone, Debug, PartialEq)]
pub enum VerdictView {
    /// Still checking: what it is at.
    Pending(String),
    Said {
        title: &'static str,
        green: bool,
        body: String,
    },
}

/// The card, as plain data, for the library's edition.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub struct ApprovalView {
    pub title: String,
    pub asked: String,
    pub verdict: VerdictView,
    /// Claude's findings: ("file lines", why).
    pub findings: Vec<(String, String)>,
    /// What the release lists, and its version code.
    pub release: Option<(Vec<String>, u32)>,
    /// What the checks are at while they run.
    pub checking: Option<String>,
    /// Each check: (passed, line).
    pub checks: Vec<(bool, String)>,
    /// Green and every check passed: the button works.
    pub ready: bool,
    /// Waiting for the fingerprint.
    pub signing: bool,
    /// Settled and not green: say there is nothing to approve.
    pub not_green: bool,
}

/// Developer tools ▸ Midas ▸ Release key as plain data ([`key_view`]).
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub struct KeySectionView {
    /// The public key, whole (for Copy), or `None` before one is made.
    pub public: Option<String>,
    /// The key shortened as the section shows it.
    pub short: String,
    /// Where it is kept, in words.
    pub kept: String,
    /// The Claude API key note.
    pub api_note: &'static str,
    pub message: Option<String>,
}

/// A tap in the Release key section.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub enum KeyAct {
    Make,
    /// Keep this Claude API key (empty forgets it).
    SaveApiKey(String),
}

/// The section's words before a key is made.
pub const KEY_MAKE_NOTE: &str = "Approves releases of the app and of self-hosted servers with your fingerprint. Made once, on this phone; it never leaves its security chip.";
/// The section's words under the key.
pub const KEY_TOOL_NOTE: &str = "A session reads it with the link's release_key tool and builds it into the server and the app as the key they trust.";

#[derive(Clone, Default)]
struct KeyView {
    public: Option<String>,
    location: Option<String>,
    message: Option<String>,
    /// Whether a Claude API key is kept.
    has_api_key: bool,
}

static STATE: Mutex<State> = Mutex::new(State {
    request: None,
    outcome: None,
    key: None,
    lib_view: None,
    lib_acts: Vec::new(),
});

/// The card as published this frame, for the library.
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub fn lib_view() -> Option<ApprovalView> {
    state().lib_view.clone()
}

/// A tap on the library's card, done next frame.
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub fn lib_act(a: ApprovalAct) {
    state().lib_acts.push(a);
}

fn state() -> std::sync::MutexGuard<'static, State> {
    STATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The link's `approve_release`: fetch and check the release at `source`, have
/// Claude review it, and put it in front of the owner. `since` is where the
/// review starts the first time; after that, the last commit this phone
/// approved. Refused while another is open.
pub fn request(session: &str, source: &str, since: Option<&str>) -> Result<(), String> {
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
    if release_key::api_key().is_none() {
        return Err(
            "no Claude API key on this phone yet: Developer tools ▸ Midas ▸ Release key".into(),
        );
    }
    // The phone's own memory wins: a session cannot move where the review
    // starts once there is one.
    let since = release_key::last_approved().or_else(|| since.map(str::to_string));
    {
        let mut s = state();
        if s.request.is_some() {
            return Err("another release is waiting for the owner already".into());
        }
        s.request = Some(Request {
            session: session.to_string(),
            source: source.clone(),
            since,
            manifest: Vec::new(),
            release: None,
            checks: Vec::new(),
            review: Review::Pending("waiting for the files to be checked".into()),
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
    let files_ok = !checks.is_empty() && checks.iter().all(|c| c.ok);
    let since = state().request.as_ref().and_then(|r| r.since.clone());
    let commit = release.commit.clone();
    let name = release.name.clone();
    finish(manifest, Some(release), checks);
    let review = if files_ok {
        review(&name, since.as_deref(), commit.as_deref())
    } else {
        Review::Failed("not asked: a file check failed".into())
    };
    if let Some(r) = state().request.as_mut() {
        r.review = review;
    }
}

/// Have Claude read the changes from `since` to `commit`, as the repository
/// holds them under those ids.
fn review(name: &str, since: Option<&str>, commit: Option<&str>) -> Review {
    let is_id =
        |c: &str| c.len() >= 7 && c.len() <= 40 && c.chars().all(|ch| ch.is_ascii_hexdigit());
    let Some(commit) = commit.filter(|c| is_id(c)) else {
        return Review::Failed(
            "release.json names no commit, so there is nothing to review".into(),
        );
    };
    let Some(since) = since.filter(|c| is_id(c)) else {
        return Review::Failed(
            "no earlier approved release to start from: ask with `since`, the commit of the last release".into(),
        );
    };
    let Some(api_key) = release_key::api_key() else {
        return Review::Failed("no Claude API key on this phone".into());
    };
    set_review(Review::Pending(format!(
        "Fetching the changes since {}",
        &since[..7]
    )));
    let url = format!("{REPOSITORY}/compare/{since}...{commit}.diff");
    let diff = match agent().get(&url).call() {
        Ok(r) => {
            let mut body = Vec::new();
            match r
                .into_reader()
                .take(release_review::MAX_DIFF_BYTES as u64 + 1)
                .read_to_end(&mut body)
            {
                Ok(_) if body.len() > release_review::MAX_DIFF_BYTES => {
                    return Review::Failed(
                        "the release changes too much to review in one pass".into(),
                    )
                }
                Ok(_) => String::from_utf8_lossy(&body).into_owned(),
                Err(e) => return Review::Failed(format!("could not read the changes: {e}")),
            }
        }
        Err(e) => return Review::Failed(format!("could not fetch the changes: {e}")),
    };
    if diff.trim().is_empty() {
        return Review::Failed("no changes between those commits".into());
    }
    set_review(Review::Pending(format!(
        "Claude is reading {} KB of changes",
        diff.len() / 1024
    )));
    match release_review::ask(
        &api_key,
        &release_review::request_body(name, since, commit, &diff),
    ) {
        Ok(v) => Review::Done(v),
        Err(e) => Review::Failed(e),
    }
}

fn set_review(review: Review) {
    if let Some(r) = state().request.as_mut() {
        r.review = review;
    }
}

fn finish(manifest: Vec<u8>, release: Option<Release>, checks: Vec<Check>) {
    if let Some(r) = state().request.as_mut() {
        if release.is_none() {
            r.review = Review::Failed("not asked: release.json could not be read".into());
        }
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

/// Developer tools ▸ Midas ▸ Release key as the library shows it, read once;
/// `None` where releases are not approved (not the owner's Android phone).
pub fn key_view() -> Option<KeySectionView> {
    if !release_key::available() {
        return None;
    }
    let k = {
        let mut s = state();
        s.key
            .get_or_insert_with(|| KeyView {
                public: release_key::public_key().ok().flatten(),
                location: release_key::location(),
                has_api_key: release_key::api_key().is_some(),
                ..KeyView::default()
            })
            .clone()
    };
    let short = k.public.as_deref().map_or_else(String::new, |key| {
        if key.len() > 24 {
            format!("{}…{}", &key[..16], &key[key.len() - 8..])
        } else {
            key.to_string()
        }
    });
    let kept = format!(
        "Kept in {}; signs only after your fingerprint.",
        match k.location.as_deref() {
            Some("strongbox") => "the phone's separate security chip (StrongBox)",
            Some("tee") => "the phone's secure area (TEE)",
            Some("software") => "software only — do not sign releases with it",
            _ => "the Android Keystore",
        }
    );
    let api_note = if k.has_api_key {
        "Claude API key: kept on this phone, encrypted. Claude reviews every release before you are asked to approve it."
    } else {
        "Claude API key: none yet. Claude reviews every release with it, straight from this phone, before you are asked to approve it."
    };
    Some(KeySectionView {
        public: k.public,
        short,
        kept,
        api_note,
        message: k.message,
    })
}

/// Make the release key or keep a Claude API key: what the library's buttons
/// do.
pub fn key_act(a: KeyAct) {
    match a {
        KeyAct::Make => {
            let made = release_key::create();
            let mut s = state();
            let k = s.key.get_or_insert_with(KeyView::default);
            match made {
                Ok(public) => {
                    k.public = Some(public);
                    k.location = release_key::location();
                    k.message = None;
                }
                Err(e) => k.message = Some(format!("Could not make it: {e}")),
            }
        }
        KeyAct::SaveApiKey(draft) => {
            let saved = release_key::save_api_key(&draft);
            let mut s = state();
            let k = s.key.get_or_insert_with(KeyView::default);
            match saved {
                Ok(()) => {
                    k.has_api_key = !draft.trim().is_empty();
                    k.message = None;
                }
                Err(e) => k.message = Some(format!("Could not keep it: {e}")),
            }
        }
    }
}

/// The release in front of the owner, over everything; and the fingerprint
/// sheet's outcome once it is answered. Publishes the card for
/// `lib_release_approval` and does its taps.
pub fn drive_release_approval() {
    // The library's taps of last frame.
    let acts = std::mem::take(&mut state().lib_acts);
    for a in acts {
        let open = state().request.clone();
        if let Some(request) = open {
            act(&request, a);
        }
    }
    let open = state().request.clone();
    let Some(request) = open else {
        // `answer` cleared the view with the request; nothing else does.
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
                if let Some(commit) = request.release.as_ref().and_then(|r| r.commit.as_deref()) {
                    release_key::save_last_approved(commit);
                }
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
    }
    // The checks and the review run on a thread: their progress shows as the
    // view moves.
    let view = approval_view(&request);
    let mut s = state();
    if s.lib_view.as_ref() != Some(&view) {
        s.lib_view = Some(view);
    }
}

/// Under the button when there is nothing to approve.
pub const NOT_GREEN: &str =
    "Not green, so there is nothing to approve. Ask a session to look into it.";

/// Green means: every file checked out on this phone, and Claude found
/// nothing. Anything else has no button.
fn ready(request: &Request) -> bool {
    let green = matches!(&request.review, Review::Done(v) if v.green());
    request.phase == Phase::Ready
        && request.release.is_some()
        && !request.checks.is_empty()
        && request.checks.iter().all(|c| c.ok)
        && green
}

/// Settled, ready to sign, and not green: the card says why there is no
/// button to press.
fn not_green(request: &Request) -> bool {
    let settled = !matches!(request.review, Review::Pending(_));
    request.phase == Phase::Ready && !ready(request) && settled
}

/// A tap on Approve or Not now.
fn act(request: &Request, a: ApprovalAct) {
    match a {
        ApprovalAct::Approve => {
            if !ready(request) {
                return;
            }
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
        ApprovalAct::Decline => {
            let name = request
                .release
                .as_ref()
                .map_or_else(|| request.source.clone(), |r| r.name.clone());
            answer(
                request,
                format!("The owner did not approve release {name}."),
            );
        }
    }
}

/// The card's title: the release's name once it reads.
fn approval_title(request: &Request) -> String {
    request.release.as_ref().map_or_else(
        || "A release for your approval".to_string(),
        |r| format!("Release {}", r.name),
    )
}

/// Claude's verdict in the card's words.
fn verdict_view(review: &Review) -> VerdictView {
    match review {
        Review::Pending(what) => VerdictView::Pending(what.clone()),
        Review::Done(v) if v.green() => VerdictView::Said {
            title: "Claude found nothing to worry about",
            green: true,
            body: v.summary.clone(),
        },
        Review::Done(v) => VerdictView::Said {
            title: "Claude found something",
            green: false,
            body: v.summary.clone(),
        },
        Review::Failed(why) => VerdictView::Said {
            title: "Not checked by Claude",
            green: false,
            body: why.clone(),
        },
    }
}

/// The card as plain data, for the library.
fn approval_view(request: &Request) -> ApprovalView {
    let findings = match &request.review {
        Review::Done(v) => v
            .findings
            .iter()
            .map(|f| (format!("{} {}", f.file, f.lines), f.why.clone()))
            .collect(),
        _ => Vec::new(),
    };
    ApprovalView {
        title: approval_title(request),
        asked: format!("Asked by {} · {}", request.session, request.source),
        verdict: verdict_view(&request.review),
        findings,
        release: request
            .release
            .as_ref()
            .map(|r| (r.changes.clone(), r.code)),
        checking: match &request.phase {
            Phase::Checking(what) => Some(what.clone()),
            _ => None,
        },
        checks: request
            .checks
            .iter()
            .map(|c| (c.ok, c.line.clone()))
            .collect(),
        ready: ready(request),
        signing: request.phase == Phase::Signing,
        not_green: not_green(request),
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
    s.lib_view = None;
    s.outcome = Some((request.session.clone(), words));
}
