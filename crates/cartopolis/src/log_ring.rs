//! The last lines of this run's log, kept in memory for the MCP `app_log`.
//!
//! **Desktop only.** On Android the log already has a home the app can read
//! back, logcat (`platform::android::own_logcat`), and `app_log` reads that; on
//! the web the browser's console has it. A desktop log goes to stderr, which a
//! session driving the client over its socket cannot see, so this layer keeps a
//! copy of it.
//!
//! Its own filter, not the stderr one: stderr is `off` outside this crate, and a
//! warning from wgpu or a shader compile is exactly what a session asks the log
//! for (`shaders-are-wesl.md`: a shader error shows only in a run's log).

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::sync::Mutex;

use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::{Context, Layer};

/// Lines kept. A busy minute of streaming at `info` is a few hundred.
const KEPT: usize = 2000;

/// One logged line, numbered from the start of the run.
#[derive(Clone)]
pub struct Line {
    pub seq: u64,
    pub level: Level,
    pub text: String,
}

struct Ring {
    next: u64,
    lines: VecDeque<Line>,
}

static RING: Mutex<Ring> = Mutex::new(Ring {
    next: 1,
    lines: VecDeque::new(),
});

/// The layer, with the filter it is meant to run under.
pub fn layer<S>() -> impl Layer<S>
where
    S: Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    use tracing_subscriber::Layer as _;
    RingLayer.with_filter(tracing_subscriber::EnvFilter::new(
        "warn,cartopolis=info,bevy_render=warn",
    ))
}

/// Every kept line after `after`, oldest first.
pub fn since(after: u64) -> Vec<Line> {
    let ring = RING.lock().unwrap_or_else(|p| p.into_inner());
    ring.lines
        .iter()
        .filter(|l| l.seq > after)
        .cloned()
        .collect()
}

struct RingLayer;

impl<S: Subscriber> Layer<S> for RingLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        let mut text = format!("{} {}:", meta.level(), meta.target());
        event.record(&mut Fields(&mut text));
        let mut ring = RING.lock().unwrap_or_else(|p| p.into_inner());
        let seq = ring.next;
        ring.next += 1;
        ring.lines.push_back(Line {
            seq,
            level: *meta.level(),
            text,
        });
        while ring.lines.len() > KEPT {
            ring.lines.pop_front();
        }
    }
}

/// `message` bare, every other field as `name=value`, as the stderr log has it.
struct Fields<'a>(&'a mut String);

impl Visit for Fields<'_> {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.0, " {value:?}");
        } else {
            let _ = write!(self.0, " {}={value:?}", field.name());
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            let _ = write!(self.0, " {value}");
        } else {
            let _ = write!(self.0, " {}={value}", field.name());
        }
    }
}
