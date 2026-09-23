//! The pipeline runner the console drives. Ported from cartopy's `server/jobs.ts`.
//!
//! **One job at a time, and one dataset at a time inside it.** Not a concurrency
//! limit that happens to be 1: these extractors page somebody else's open-data
//! services, several thousand requests each, and running two at once doubles the
//! load PDOK sees from one address for no wall-clock gain worth having.
//!
//! The runner owns one plain thread. A dataset is a blocking function, and giving it
//! a thread of its own rather than tokio's blocking pool keeps a twenty-minute BGT
//! run from ever being mistaken for a short blocking call.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::pipeline::{Count, Ctx, DatasetResult, Outcome, PipelineError, Progress, Sink};

/// How many log lines a job keeps. A BAG run prints a line per page and a city is
/// tens of thousands of pages, so the log is unbounded in principle and this is
/// what stops one run from being the process's memory profile. The console shows
/// the tail; the warnings and the totals are at the end by construction.
pub const LOG_LIMIT: usize = 2_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    Queued,
    Running,
    Done,
    Failed,
    Cancelled,
}

impl JobStatus {
    fn finished(self) -> bool {
        matches!(self, Self::Done | Self::Failed | Self::Cancelled)
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct LogLine {
    pub at: u64,
    /// Which dataset said it — a job runs several and the console interleaves them.
    pub dataset: String,
    pub text: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobStep {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub counts: Option<Vec<Count>>,
    pub dataset: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<u64>,
    pub progress: Progress,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<u64>,
    pub status: JobStatus,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    pub area: String,
    pub bbox: [f64; 4],
    /// Who pressed the button, for the history list.
    pub by: String,
    pub created_at: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<u64>,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<u64>,
    pub status: JobStatus,
    pub steps: Vec<JobStep>,
}

/// One runnable dataset, as the runner sees it.
#[derive(Clone)]
pub struct Runnable {
    pub label: String,
    pub run: Arc<dyn Fn(&Ctx) -> Outcome<DatasetResult> + Send + Sync>,
}

/// The datasets a runner may run. A constructor argument rather than a global for
/// one reason: a test that submits a job must not page somebody else's service.
pub type Registry = HashMap<String, Runnable>;

/// Every dataset in [`crate::pipeline::DATASETS`] — what every real caller wants.
#[must_use]
pub fn default_registry() -> Registry {
    crate::pipeline::DATASETS
        .iter()
        .map(|d| {
            (
                d.id.to_string(),
                Runnable {
                    label: d.label.to_string(),
                    run: Arc::new(d.run),
                },
            )
        })
        .collect()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

struct Entry {
    job: Job,
    log: VecDeque<LogLine>,
    /// Lines dropped off the front, so `since` stays a count of lines ever written
    /// and a follower does not re-read the tail once the ring starts to turn.
    dropped: usize,
    cancel: Arc<AtomicBool>,
}

#[derive(Default)]
struct Inner {
    /// In submission order; `list` reverses it.
    jobs: Vec<Entry>,
    queue: VecDeque<String>,
    next_id: u64,
}

impl Inner {
    fn entry(&mut self, id: &str) -> Option<&mut Entry> {
        self.jobs.iter_mut().find(|e| e.job.id == id)
    }

    fn append(&mut self, id: &str, dataset: &str, text: String) {
        if let Some(e) = self.entry(id) {
            e.log.push_back(LogLine {
                at: now_ms(),
                dataset: dataset.to_string(),
                text,
            });
            while e.log.len() > LOG_LIMIT {
                e.log.pop_front();
                e.dropped += 1;
            }
        }
    }
}

struct Shared {
    inner: Mutex<Inner>,
    wake: Condvar,
    out_root: PathBuf,
    registry: Registry,
}

/// The runner. Cheap to clone; every clone is the same queue.
#[derive(Clone)]
pub struct Runner {
    shared: Arc<Shared>,
}

impl Runner {
    /// Start the runner's thread. It lives as long as the process.
    #[must_use]
    pub fn new(out_root: &Path, registry: Registry) -> Self {
        let shared = Arc::new(Shared {
            inner: Mutex::new(Inner {
                next_id: 1,
                ..Inner::default()
            }),
            wake: Condvar::new(),
            out_root: out_root.to_path_buf(),
            registry,
        });
        let worker = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("atlas-pipeline".into())
            .spawn(move || drain(&worker))
            .expect("spawn the pipeline runner thread");
        Self { shared }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.shared
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Newest first — the console's history is read from the top.
    #[must_use]
    pub fn list(&self) -> Vec<Job> {
        self.lock()
            .jobs
            .iter()
            .rev()
            .map(|e| e.job.clone())
            .collect()
    }

    #[must_use]
    pub fn get(&self, id: &str) -> Option<Job> {
        self.lock().entry(id).map(|e| e.job.clone())
    }

    /// Everything after the first `since` lines, and how many lines there have
    /// been in all. `since` is a line count, not a timestamp: the console holds what
    /// it has drawn and asks for the rest, so a quiet minute costs an empty array.
    #[must_use]
    pub fn log(&self, id: &str, since: usize) -> Option<(Vec<LogLine>, usize)> {
        let mut inner = self.lock();
        let e = inner.entry(id)?;
        let total = e.dropped + e.log.len();
        let skip = since.saturating_sub(e.dropped);
        Some((e.log.iter().skip(skip).cloned().collect(), total))
    }

    pub fn submit(&self, area: &str, bbox: [f64; 4], datasets: &[String], by: &str) -> Job {
        let mut inner = self.lock();
        let id = inner.next_id.to_string();
        inner.next_id += 1;
        let job = Job {
            area: area.to_string(),
            bbox,
            by: by.to_string(),
            created_at: now_ms(),
            finished_at: None,
            id: id.clone(),
            started_at: None,
            status: JobStatus::Queued,
            steps: datasets
                .iter()
                .map(|d| JobStep {
                    counts: None,
                    dataset: d.clone(),
                    error: None,
                    finished_at: None,
                    progress: Progress::default(),
                    started_at: None,
                    status: JobStatus::Queued,
                })
                .collect(),
        };
        inner.jobs.push(Entry {
            job: job.clone(),
            log: VecDeque::new(),
            dropped: 0,
            cancel: Arc::new(AtomicBool::new(false)),
        });
        inner.queue.push_back(id);
        drop(inner);
        self.shared.wake.notify_one();
        job
    }

    /// Stop a job. A queued one never starts; a running one unwinds at its next
    /// cancellation point, which is inside every paging loop — so a stop takes
    /// effect within one request, not at the end of a twenty-minute layer.
    ///
    /// What has been written stays written. Every file is written whole and the
    /// coverage manifest last, so a cancelled run is a *mixture* of two runs, never
    /// a broken one, and it leaves the previous manifest in place.
    pub fn cancel(&self, id: &str) -> bool {
        let mut inner = self.lock();
        let Some(e) = inner.entry(id) else {
            return false;
        };
        if e.job.status.finished() {
            return false;
        }
        e.cancel.store(true, Ordering::Relaxed);
        if e.job.status == JobStatus::Queued {
            e.job.status = JobStatus::Cancelled;
            e.job.finished_at = Some(now_ms());
            for step in &mut e.job.steps {
                step.status = JobStatus::Cancelled;
            }
            inner.queue.retain(|q| q != id);
        }
        true
    }
}

/// Where a running step's log lines and progress go: back into the job's entry.
struct StepSink {
    shared: Arc<Shared>,
    job: String,
    step: usize,
    dataset: String,
}

impl Sink for StepSink {
    fn log(&self, line: String) {
        let mut inner = self.shared.inner.lock().unwrap_or_else(|p| p.into_inner());
        inner.append(&self.job, &self.dataset, line);
    }

    fn progress(&self, update: Progress) {
        let mut inner = self.shared.inner.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(step) = inner
            .entry(&self.job)
            .and_then(|e| e.job.steps.get_mut(self.step))
        {
            step.progress.merge(update);
        }
    }
}

fn drain(shared: &Arc<Shared>) {
    loop {
        let id = {
            let mut inner = shared.inner.lock().unwrap_or_else(|p| p.into_inner());
            loop {
                if let Some(id) = inner.queue.pop_front() {
                    break id;
                }
                inner = shared.wake.wait(inner).unwrap_or_else(|p| p.into_inner());
            }
        };
        run_job(shared, &id);
    }
}

fn run_job(shared: &Arc<Shared>, id: &str) {
    let lock = || shared.inner.lock().unwrap_or_else(|p| p.into_inner());
    let (cancel, area, bbox, datasets) = {
        let mut inner = lock();
        let Some(e) = inner.entry(id) else { return };
        e.job.status = JobStatus::Running;
        e.job.started_at = Some(now_ms());
        (
            Arc::clone(&e.cancel),
            e.job.area.clone(),
            e.job.bbox,
            e.job
                .steps
                .iter()
                .map(|s| s.dataset.clone())
                .collect::<Vec<_>>(),
        )
    };

    for (index, dataset) in datasets.iter().enumerate() {
        let set = |f: &dyn Fn(&mut JobStep)| {
            if let Some(step) = lock().entry(id).and_then(|e| e.job.steps.get_mut(index)) {
                f(step);
            }
        };
        if cancel.load(Ordering::Relaxed) {
            set(&|s| s.status = JobStatus::Cancelled);
            continue;
        }
        let Some(runnable) = shared.registry.get(dataset) else {
            set(&|s| {
                s.status = JobStatus::Failed;
                s.error = Some(format!("unknown dataset '{dataset}'"));
            });
            continue;
        };
        set(&|s| {
            s.status = JobStatus::Running;
            s.started_at = Some(now_ms());
        });
        lock().append(
            id,
            dataset,
            format!(
                "{} — {area} [{}]",
                runnable.label,
                bbox.map(|v| v.to_string()).join(",")
            ),
        );
        let ctx = Ctx::new(
            bbox,
            &area,
            &shared.out_root,
            Arc::clone(&cancel),
            Arc::new(StepSink {
                shared: Arc::clone(shared),
                job: id.to_string(),
                step: index,
                dataset: dataset.clone(),
            }),
        );
        // A panic in an extractor is a failed step, not a dead runner: the thread
        // is the only one and every later job would queue behind it forever.
        let outcome =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (runnable.run)(&ctx)))
                .unwrap_or_else(|panic| {
                    let why = panic
                        .downcast_ref::<String>()
                        .cloned()
                        .or_else(|| panic.downcast_ref::<&str>().map(|s| (*s).to_string()))
                        .unwrap_or_else(|| "panicked".to_string());
                    Err(PipelineError::Failed(format!("panicked: {why}")))
                });
        match outcome {
            Ok(result) => set(&|s| {
                s.counts = Some(result.counts.clone());
                s.status = JobStatus::Done;
            }),
            Err(PipelineError::Cancelled) => set(&|s| s.status = JobStatus::Cancelled),
            Err(PipelineError::Failed(error)) => {
                lock().append(id, dataset, format!("FAILED: {error}"));
                set(&|s| {
                    s.status = JobStatus::Failed;
                    s.error = Some(error.clone());
                });
            }
        }
        set(&|s| s.finished_at = Some(now_ms()));
    }

    let mut inner = lock();
    if let Some(e) = inner.entry(id) {
        e.job.finished_at = Some(now_ms());
        // The worst of its steps: one failed dataset out of four is a failed run,
        // because the area is then partly refreshed and somebody has to know which.
        let any = |st| e.job.steps.iter().any(|s| s.status == st);
        e.job.status = if any(JobStatus::Failed) {
            JobStatus::Failed
        } else if any(JobStatus::Cancelled) {
            JobStatus::Cancelled
        } else {
            JobStatus::Done
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    const BBOX: [f64; 4] = [229_000.0, 578_000.0, 230_000.0, 579_000.0];

    fn registry(run: impl Fn(&Ctx) -> Outcome<DatasetResult> + Send + Sync + 'static) -> Registry {
        HashMap::from([(
            "buildings".to_string(),
            Runnable {
                label: "stub".into(),
                run: Arc::new(run),
            },
        )])
    }

    /// Runs until cancelled — the shape of a long dataset.
    fn forever(ctx: &Ctx) -> Outcome<DatasetResult> {
        loop {
            ctx.sleep(Duration::from_millis(20))?;
        }
    }

    fn wait_for(runner: &Runner, id: &str, status: JobStatus) -> Job {
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            let job = runner.get(id).unwrap();
            if job.status == status || Instant::now() > until {
                return job;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn ids(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn a_queued_job_cancels_without_ever_starting() {
        let dir = tempfile::tempdir().unwrap();
        let runner = Runner::new(dir.path(), registry(forever));
        let first = runner.submit("a", BBOX, &ids(&["buildings"]), "tester");
        let second = runner.submit("b", BBOX, &ids(&["buildings"]), "tester");
        assert!(runner.cancel(&second.id));
        let job = runner.get(&second.id).unwrap();
        assert_eq!(job.status, JobStatus::Cancelled);
        assert_eq!(job.steps[0].status, JobStatus::Cancelled);
        // Cancelling it twice is not an error the console has to explain.
        assert!(!runner.cancel(&second.id));

        // And a running one unwinds at its next check.
        wait_for(&runner, &first.id, JobStatus::Running);
        assert!(runner.cancel(&first.id));
        assert_eq!(
            wait_for(&runner, &first.id, JobStatus::Cancelled).status,
            JobStatus::Cancelled
        );
    }

    #[test]
    fn an_unknown_dataset_fails_its_step_not_the_runner() {
        let dir = tempfile::tempdir().unwrap();
        let runner = Runner::new(dir.path(), registry(|_| Ok(DatasetResult::default())));
        let bad = runner.submit("a", BBOX, &ids(&["nope"]), "tester");
        let job = wait_for(&runner, &bad.id, JobStatus::Failed);
        assert_eq!(job.status, JobStatus::Failed);
        assert!(job.steps[0].error.as_deref().unwrap().contains("nope"));

        let good = runner.submit("a", BBOX, &ids(&["buildings"]), "tester");
        assert_eq!(
            wait_for(&runner, &good.id, JobStatus::Done).status,
            JobStatus::Done
        );
    }

    #[test]
    fn a_panicking_dataset_is_a_failed_step() {
        let dir = tempfile::tempdir().unwrap();
        let runner = Runner::new(dir.path(), registry(|_| panic!("boom")));
        let job = runner.submit("a", BBOX, &ids(&["buildings"]), "tester");
        let job = wait_for(&runner, &job.id, JobStatus::Failed);
        assert!(job.steps[0].error.as_deref().unwrap().contains("boom"));
        let next = runner.submit("a", BBOX, &ids(&["buildings"]), "tester");
        assert_eq!(
            wait_for(&runner, &next.id, JobStatus::Failed).status,
            JobStatus::Failed
        );
    }

    #[test]
    fn the_log_is_followed_by_line_count_and_survives_the_ring_turning() {
        let dir = tempfile::tempdir().unwrap();
        let runner = Runner::new(
            dir.path(),
            registry(|ctx| {
                for n in 0..LOG_LIMIT + 10 {
                    ctx.log(format!("line {n}"));
                }
                ctx.progress(Progress {
                    phase: Some("panden".into()),
                    done: Some(3),
                    total: Some(None),
                });
                Ok(DatasetResult {
                    cells: 1,
                    counts: vec![Count::new("buildings", 7)],
                })
            }),
        );
        let job = runner.submit("a", BBOX, &ids(&["buildings"]), "tester");
        let job = wait_for(&runner, &job.id, JobStatus::Done);
        assert_eq!(job.steps[0].counts.as_ref().unwrap()[0].n, 7);
        assert_eq!(job.steps[0].progress.done, Some(3));

        let (lines, total) = runner.log(&job.id, 0).unwrap();
        // The header line plus LOG_LIMIT + 10, of which the ring keeps the last.
        assert_eq!(total, LOG_LIMIT + 11);
        assert_eq!(lines.len(), LOG_LIMIT);
        assert_eq!(
            lines.last().unwrap().text,
            format!("line {}", LOG_LIMIT + 9)
        );
        let (rest, _) = runner.log(&job.id, total).unwrap();
        assert!(rest.is_empty());
        let (two, _) = runner.log(&job.id, total - 2).unwrap();
        assert_eq!(two.len(), 2);

        let json = serde_json::to_value(&job).unwrap();
        assert_eq!(json["status"], "done");
        assert!(json["createdAt"].is_u64());
        assert_eq!(
            json["steps"][0]["progress"]["total"],
            serde_json::Value::Null
        );
    }
}
