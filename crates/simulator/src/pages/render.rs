//! The content pages' render process: every visit to a live page is drawn to
//! HTML on the server, by the web UI's own views (`docs/mockups/cms`, v3).
//!
//! The views are the web UI's `pages-ssr.js`, found in `web_root` beside the
//! `pages.html` and `pages-<hash>.js` of the same build, so the HTML always
//! matches the script that takes it over in the browser. They run in the script
//! host's executable started as `render <bundle>`
//! (`tools/script-host/src/render.ts`) — the same Bun, never the same process:
//! that one runs players' code, and what this one writes is the site.
//!
//! One thread owns the process and asks it one page at a time; a render is
//! tens of microseconds (`docs/mockups/cms`). Whatever goes wrong — no bundle,
//! no executable, a crash, an answer that does not come — the visit gets the
//! page without its body, which the browser draws as it did before this
//! existed. A hung process is killed and started again with the next visit; a
//! new bundle (a web deploy) is noticed by its modification time and starts a
//! new process.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::time::{Duration, Instant, SystemTime};

use serde_json::Value;
use tokio::sync::oneshot;

use super::PageData;
use crate::scripting::BunEngine;

/// The server build of the pages' views, in `web_root` (`build.ts`).
pub const BUNDLE: &str = "pages-ssr.js";

/// How long one page may take, the process's start included (~60 ms warm,
/// ~0.5 s on a cold disk). A visit waits no longer than this for its body.
const ANSWER_WAIT: Duration = Duration::from_secs(2);

/// After a failed start, how long visits go without asking for another.
const RETRY_AFTER: Duration = Duration::from_secs(30);

/// Visits waiting for the thread; more than this get the body-less page.
const QUEUE: usize = 64;

struct Job {
    page: String,
    reply: oneshot::Sender<Option<String>>,
}

/// A handle on the render thread; cheap to clone.
#[derive(Clone)]
pub struct Renderer {
    jobs: Option<SyncSender<Job>>,
}

impl Renderer {
    /// A renderer that draws nothing: every page goes out for the browser to draw.
    #[must_use]
    pub fn off() -> Self {
        Self { jobs: None }
    }

    /// The thread, with the process started at once when the bundle is there,
    /// so the first visit does not wait for it.
    #[must_use]
    pub fn start(engine: BunEngine, bundle: PathBuf) -> Self {
        let (tx, rx) = mpsc::sync_channel(QUEUE);
        let spawned = std::thread::Builder::new()
            .name("cartopolis-pages-render".into())
            .spawn(move || Worker::new(engine, bundle).run(&rx));
        match spawned {
            Ok(_) => Self { jobs: Some(tx) },
            Err(e) => {
                tracing::warn!(error = %e, "content pages: no render thread");
                Self::off()
            }
        }
    }

    /// The body of `page` as HTML, or `None` for the browser to draw it.
    pub async fn render(&self, page: &PageData) -> Option<String> {
        let jobs = self.jobs.as_ref()?;
        let page = serde_json::to_string(page).ok()?;
        let (reply, answer) = oneshot::channel();
        match jobs.try_send(Job { page, reply }) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                tracing::warn!("content pages: renderer busy; page left to the browser");
                return None;
            }
            Err(TrySendError::Disconnected(_)) => return None,
        }
        // The thread answers every job, within ANSWER_WAIT of reaching it.
        answer.await.ok().flatten()
    }
}

struct Process {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    /// The bundle it loaded, by modification time.
    stamp: SystemTime,
}

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Worker {
    engine: BunEngine,
    bundle: PathBuf,
    process: Option<Process>,
    failed_at: Option<Instant>,
    next_id: u64,
}

impl Worker {
    fn new(engine: BunEngine, bundle: PathBuf) -> Self {
        Self {
            engine,
            bundle,
            process: None,
            failed_at: None,
            next_id: 0,
        }
    }

    fn run(mut self, jobs: &Receiver<Job>) {
        self.ready();
        for job in jobs {
            let html = self.render(&job.page);
            let _ = job.reply.send(html);
        }
    }

    /// A process for the bundle as it is now, started if need be.
    fn ready(&mut self) -> Option<&mut Process> {
        let Some(stamp) = std::fs::metadata(&self.bundle)
            .and_then(|m| m.modified())
            .ok()
        else {
            self.process = None;
            return None;
        };
        let stale = self
            .process
            .as_mut()
            .is_some_and(|p| p.stamp != stamp || !matches!(p.child.try_wait(), Ok(None)));
        if stale {
            tracing::info!("content pages: a new bundle or a dead renderer; starting it again");
            self.process = None;
        }
        if self.process.is_none() {
            if self.failed_at.is_some_and(|t| t.elapsed() < RETRY_AFTER) {
                return None;
            }
            match spawn(&self.engine, &self.bundle, stamp) {
                Ok(p) => {
                    self.failed_at = None;
                    self.process = Some(p);
                }
                Err(e) => {
                    tracing::warn!(error = %e, bundle = %self.bundle.display(), "content pages: could not start the renderer");
                    self.failed_at = Some(Instant::now());
                }
            }
        }
        self.process.as_mut()
    }

    fn render(&mut self, page: &str) -> Option<String> {
        self.next_id += 1;
        let id = self.next_id;
        let process = self.ready()?;
        let line = format!("{{\"id\":{id},\"page\":{page}}}\n");
        if process
            .stdin
            .write_all(line.as_bytes())
            .and_then(|()| process.stdin.flush())
            .is_err()
        {
            self.process = None;
            return None;
        }
        let deadline = Instant::now() + ANSWER_WAIT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match process.lines.recv_timeout(left) {
                Ok(line) => {
                    // A line that is not this answer is an earlier, abandoned
                    // one, or something that printed to stdout regardless.
                    let Ok(answer) = serde_json::from_str::<Value>(&line) else {
                        continue;
                    };
                    if answer.get("id").and_then(Value::as_u64) != Some(id) {
                        continue;
                    }
                    if let Some(html) = answer.get("html").and_then(Value::as_str) {
                        return Some(html.to_string());
                    }
                    let error = answer.get("error").and_then(Value::as_str).unwrap_or("");
                    tracing::warn!(error, "content pages: a page did not render");
                    return None;
                }
                Err(RecvTimeoutError::Timeout) => {
                    tracing::warn!(
                        "content pages: the renderer did not answer within {} ms; restarting it",
                        ANSWER_WAIT.as_millis()
                    );
                    self.process = None;
                    return None;
                }
                Err(RecvTimeoutError::Disconnected) => {
                    tracing::warn!("content pages: the renderer exited");
                    self.process = None;
                    return None;
                }
            }
        }
    }
}

fn spawn(
    engine: &BunEngine,
    bundle: &std::path::Path,
    stamp: SystemTime,
) -> std::io::Result<Process> {
    let bundle = std::fs::canonicalize(bundle)?;
    let mut child = crate::scripting::host_command(engine)?
        .arg("render")
        .arg(&bundle)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;
    let stdin = child.stdin.take().expect("piped stdin");
    let stdout = child.stdout.take().expect("piped stdout");
    let (tx, lines) = mpsc::channel();
    std::thread::Builder::new()
        .name("cartopolis-pages-read".into())
        .spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        })?;
    tracing::info!(bundle = %bundle.display(), "content pages: renderer started");
    Ok(Process {
        child,
        stdin,
        lines,
        stamp,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn page(slug: &str) -> PageData {
        PageData {
            slug: slug.into(),
            lang: "nl".into(),
            languages: vec!["nl".into(), "en".into()],
            source_lang: "nl".into(),
            revision: Some(1),
            tree: serde_json::json!({"c": "Page"}),
            texts: BTreeMap::new(),
            menu: crate::pages::SiteMenu {
                home: false,
                pages: Vec::new(),
            },
        }
    }

    /// A stand-in for `pages-ssr.js`: the protocol is the script host's, the
    /// views are the web UI's and tested there (`ssr.test.ts`).
    fn bundle(dir: &std::path::Path, body: &str) -> PathBuf {
        let path = dir.join(BUNDLE);
        std::fs::write(&path, body).unwrap();
        path
    }

    #[tokio::test]
    async fn a_page_is_drawn_by_the_bundle() {
        let dir = tempfile::tempdir().unwrap();
        let path = bundle(
            dir.path(),
            "console.log('noise on stdout'); export async function renderPage(d) { return `<p>${d.slug}/${d.lang}</p>` }",
        );
        let r = Renderer::start(BunEngine::development(), path);
        assert_eq!(
            r.render(&page("over")).await.as_deref(),
            Some("<p>over/nl</p>")
        );
        assert_eq!(
            r.render(&page("home")).await.as_deref(),
            Some("<p>home/nl</p>")
        );
    }

    #[tokio::test]
    async fn a_page_that_throws_is_left_to_the_browser() {
        let dir = tempfile::tempdir().unwrap();
        let path = bundle(
            dir.path(),
            "export async function renderPage(d) { if (d.slug === 'bad') throw new Error('no'); return 'ok' }",
        );
        let r = Renderer::start(BunEngine::development(), path);
        assert_eq!(r.render(&page("bad")).await, None);
        assert_eq!(r.render(&page("good")).await.as_deref(), Some("ok"));
    }

    #[tokio::test]
    async fn a_hung_renderer_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = bundle(
            dir.path(),
            "export async function renderPage(d) { if (d.slug === 'hang') for (;;) {} return 'ok' }",
        );
        let r = Renderer::start(BunEngine::development(), path);
        let started = Instant::now();
        assert_eq!(r.render(&page("hang")).await, None);
        assert!(started.elapsed() < ANSWER_WAIT + Duration::from_secs(1));
        assert_eq!(r.render(&page("good")).await.as_deref(), Some("ok"));
    }

    #[tokio::test]
    async fn a_new_bundle_is_picked_up() {
        let dir = tempfile::tempdir().unwrap();
        let path = bundle(
            dir.path(),
            "export async function renderPage() { return 'one' }",
        );
        let r = Renderer::start(BunEngine::development(), path.clone());
        assert_eq!(r.render(&page("x")).await.as_deref(), Some("one"));
        // A deploy writes a new file; the stamp is its modification time.
        std::thread::sleep(Duration::from_millis(20));
        bundle(
            dir.path(),
            "export async function renderPage() { return 'two' }",
        );
        let later = SystemTime::now() + Duration::from_secs(5);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(later)
            .unwrap();
        assert_eq!(r.render(&page("x")).await.as_deref(), Some("two"));
    }

    #[tokio::test]
    async fn no_bundle_draws_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let r = Renderer::start(BunEngine::development(), dir.path().join(BUNDLE));
        assert_eq!(r.render(&page("x")).await, None);
        assert_eq!(Renderer::off().render(&page("x")).await, None);
    }
}
