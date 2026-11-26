//! The two tools about the screen itself: `ui_tree` lists the controls on the
//! screen, and `press` taps one the way a finger would.
//!
//! **Both read the component library's tree** ([`serve`],
//! `mithril_bevy::access_tree` / `press`): roles, names and rects from the
//! mounted nodes, a press as the picking backend's pointer events on what its
//! point hits, so a control under a sheet that a finger could not reach is not
//! pressed. Each control goes by a six-digit ref folded from the node's path
//! key, which is stable while the node's place in the tree is.
//!
//! **Text is withheld here as it is from a picture.** A text field's contents
//! never leave: only its label. Plain text is not a control and is not
//! listed. A clickable row with no label of its own is named by the text
//! inside it — and that text is left out while online with an allow-list
//! narrower than everyone, the rule `pump` applies to the chrome in a `view`,
//! because a row in the chat panel is somebody's words.
// Built without `ui-probe` there is no tree to read: [`serve`] refuses, and
// the answers below go unused.
#![cfg_attr(not(feature = "ui-probe"), allow(dead_code))]

use std::collections::HashSet;
#[cfg(feature = "ui-probe")]
use std::collections::VecDeque;

use bevy::prelude::*;
use futures_channel::oneshot;
use serde_json::{json, Value};

#[cfg(feature = "ui-probe")]
use super::agent::Audience;
use super::{DeviceRequest, McpBridge, Outcome, Reply};
#[cfg(feature = "ui-probe")]
use crate::systems::session::SessionState;
use cartopolis_mcp::args::AgentReply;
use cartopolis_mcp::device::{PressArgs, UiTreeArgs};

/// How long one step may wait for the UI to draw before the call is refused. A
/// headless client idles at two frames a second (`mcp::pace_idle`), and a press
/// is several frames.
const STAGE_TIMEOUT_S: f64 = 15.0;
/// Lines of a `ui_tree` answer; the data carries every control.
const LIST_MAX: usize = 120;
/// Lines of what is on screen after a `press`.
const AFTER_MAX: usize = 30;
/// Characters of one label.
const LABEL_MAX: usize = 80;
/// A control with no name larger than this share of the screen is a backdrop
/// or a container, not something to press.
const NAMELESS_MAX_SHARE: f32 = 0.25;
/// Text inside a clickable row: more pieces than this and it is a container.
const ROW_TEXT_MAX: usize = 4;

// ---------------------------------------------------------------------------
// What is on the screen
// ---------------------------------------------------------------------------

/// One control on the screen, in the library's logical px (physical px over
/// display scale × UI scale).
#[derive(Clone, Debug, PartialEq)]
pub struct Control {
    /// The library's path key (`AccessNode::key`): stable while the node's
    /// place in the tree is.
    pub id: u64,
    /// What the library presses it by: its `NodeId`, which a remount changes.
    pub handle: u64,
    pub kind: &'static str,
    /// Empty when it has none (or when it is [`Self::withheld`]).
    pub label: String,
    /// Named only by text inside it that is not shown here.
    pub withheld: bool,
    pub enabled: bool,
    /// A checkbox's tick, a selectable's selection.
    pub selected: Option<bool>,
    /// A slider's value.
    pub value: Option<f64>,
    /// A dropdown's chosen option.
    pub choice: Option<String>,
    pub rect: Rect,
    /// Where a finger lands on it: its centre, or another point of it when the
    /// centre is under another layer. `None` when all of it is covered.
    pub tap: Option<Vec2>,
}

/// Every control on the screen, in reading order.
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub screen: Rect,
    pub controls: Vec<Control>,
    pub withheld: usize,
}

fn has_toggle(kind: &str) -> bool {
    matches!(kind, "checkbox" | "switch" | "radio button")
}

/// What a control is called.
#[derive(Debug, PartialEq)]
enum Name {
    Own(String),
    /// From the text inside it.
    Inner(String),
    /// It has text inside it, and that is not shown.
    Withheld,
    /// It holds other controls or a lot of text: a container, not a control.
    Container,
    Nothing,
}

/// Cut a label to [`LABEL_MAX`] characters, on one line.
fn clip(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= LABEL_MAX {
        return flat;
    }
    let mut out: String = flat.chars().take(LABEL_MAX - 1).collect();
    out.push('…');
    out
}

/// Put controls in reading order: rows of 8 pt, then left to right.
fn reading_order(controls: &mut [Control]) {
    let row = |c: &Control| (c.rect.min.y / 8.0).floor();
    controls.sort_by(|a, b| {
        row(a)
            .total_cmp(&row(b))
            .then(a.rect.min.x.total_cmp(&b.rect.min.x))
    });
}

/// A rect with room in it: wider and taller than nothing.
fn is_positive(r: Rect) -> bool {
    r.width() > 0.0 && r.height() > 0.0
}

fn area(r: Rect) -> f32 {
    r.width() * r.height()
}

/// `inner` lies wholly inside `outer`.
fn contains_rect(outer: Rect, inner: Rect) -> bool {
    outer.contains(inner.min) && outer.contains(inner.max)
}

/// Drop a nameless control lying inside a named one: the box a map button
/// stands in senses the same click, and listed it twice over, once with the
/// button's name and once as an unlabelled area at the same place.
fn drop_shadows(controls: &mut Vec<Control>) {
    let named: Vec<Rect> = controls
        .iter()
        .filter(|c| !c.label.is_empty() || c.withheld)
        .map(|c| c.rect)
        .collect();
    controls.retain(|c| {
        !c.label.is_empty()
            || c.withheld
            || !named.iter().any(|r| contains_rect(r.inflate(1.0), c.rect))
    });
}

/// The kind a library role is shown as.
#[cfg(feature = "ui-probe")]
fn lib_kind(role: mithril_bevy::Role) -> Option<&'static str> {
    use mithril_bevy::Role as R;
    Some(match role {
        R::Button => "button",
        R::Link => "link",
        R::CheckBox => "checkbox",
        R::Switch => "switch",
        R::Radio => "radio button",
        R::Tab => "tab",
        R::TextField => "text field",
        R::Slider => "slider",
        R::Select => "dropdown",
        R::Option => "option",
        R::MenuItem => "menu item",
        R::Clickable => "clickable area",
        R::Heading | R::Image | R::Text => return None,
    })
}

/// What a library node is called: a field by its label and never by what is
/// typed in it; a control by its own name; else by the text inside it — its
/// own words when it is one piece in a button, a link, a tab or an option,
/// somebody's words (withheld while `hide_text`) when it is a row of several.
#[cfg(feature = "ui-probe")]
fn lib_name(n: &mithril_bevy::AccessNode, hide_text: bool) -> Name {
    use mithril_bevy::Role as R;
    if let Some(label) = &n.label {
        return Name::Own(label.clone());
    }
    if n.role == R::TextField {
        return Name::Nothing;
    }
    if n.holds_controls || n.text.len() > ROW_TEXT_MAX {
        return Name::Container;
    }
    if n.text.is_empty() {
        return Name::Nothing;
    }
    let one_label = n.text.len() == 1
        && matches!(
            n.role,
            R::Button
                | R::Link
                | R::Tab
                | R::Option
                | R::MenuItem
                | R::CheckBox
                | R::Radio
                | R::Switch
        );
    if one_label {
        Name::Own(n.text[0].clone())
    } else if hide_text {
        Name::Withheld
    } else {
        Name::Inner(n.text.join(" "))
    }
}

/// The library's controls as a [`Snapshot`], from its walk of the mounted
/// tree (`mithril_bevy::access_tree`).
#[cfg(feature = "ui-probe")]
fn lib_controls(nodes: &[mithril_bevy::AccessNode], screen: Rect, hide_text: bool) -> Snapshot {
    let mut out = Snapshot {
        screen,
        controls: Vec::new(),
        withheld: 0,
    };
    let mut seen = HashSet::new();
    for n in nodes {
        let Some(kind) = lib_kind(n.role) else {
            continue;
        };
        let rect = n.rect.intersect(screen);
        if !is_positive(rect) {
            continue;
        }
        let (label, withheld) = match lib_name(n, hide_text) {
            Name::Container => continue,
            Name::Nothing if area(rect) > NAMELESS_MAX_SHARE * area(screen) => continue,
            Name::Nothing => (String::new(), false),
            Name::Own(text) | Name::Inner(text) => (clip(&text), false),
            Name::Withheld => (String::new(), true),
        };
        // Two nodes in the same place in two mounts of one view.
        if !seen.insert(n.key) {
            continue;
        }
        out.withheld += usize::from(withheld);
        let value = (n.role == mithril_bevy::Role::Slider)
            .then(|| n.value.as_deref().and_then(|v| v.parse::<f64>().ok()))
            .flatten();
        let choice = (n.role == mithril_bevy::Role::Select)
            .then(|| n.value.clone())
            .flatten();
        out.controls.push(Control {
            id: n.key,
            handle: n.node.0,
            kind,
            label,
            withheld,
            enabled: n.enabled,
            selected: n.checked,
            value,
            choice,
            rect,
            tap: n.tap.filter(|p| screen.contains(*p)),
        });
    }
    drop_shadows(&mut out.controls);
    reading_order(&mut out.controls);
    out
}

/// The library's screen now, or `None` when it has no root.
#[cfg(feature = "ui-probe")]
fn lib_snapshot(world: &World, hide_text: bool) -> Option<Snapshot> {
    let screen = mithril_bevy::screen_rect(world)?;
    Some(lib_controls(
        &mithril_bevy::access_tree(world),
        screen,
        hide_text,
    ))
}

/// How a press is made.
#[derive(Clone, Debug)]
struct Plan {
    control: Control,
    at: Vec2,
    /// As a finger, or as a mouse.
    touch: bool,
    long: bool,
}

impl Plan {
    fn how(&self) -> &'static str {
        match (self.long, self.touch) {
            (false, _) => "tapped",
            (true, true) => "long-pressed",
            (true, false) => "right-clicked (no touch screen here, so that is the long press)",
        }
    }
}

// ---------------------------------------------------------------------------
// Finding a control
// ---------------------------------------------------------------------------

/// The short number a control goes by in `ui_tree`: six digits folded from its
/// id, because a model copies 6 digits more reliably than 20. A clash is
/// caught in [`resolve`].
pub fn short_ref(id: u64) -> u64 {
    ((id >> 32) ^ (id & 0xFFFF_FFFF)) % 1_000_000
}

/// Lowercase words, with the icons and punctuation round them dropped, so
/// "⚙ Settings" is "settings".
fn normalize(text: &str) -> String {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join(" ")
}

enum Target {
    Ref(u64),
    Label(String),
}

fn target_of(args: &PressArgs) -> Result<Target, &'static str> {
    let label = args
        .label
        .as_deref()
        .map(str::trim)
        .filter(|l| !l.is_empty());
    match (args.r#ref, label) {
        (Some(r), _) => Ok(Target::Ref(r)),
        (None, Some(l)) => Ok(Target::Label(l.to_string())),
        (None, None) => Err("Say which control to press: a `ref` from `ui_tree`, or its label."),
    }
}

/// `#123456 "Label"`, for lists of candidates.
fn mention(c: &Control) -> String {
    if c.label.is_empty() {
        format!("#{} {}", short_ref(c.id), c.kind)
    } else {
        format!("#{} \"{}\"", short_ref(c.id), c.label)
    }
}

/// Labelled controls that look like what was asked for, best first.
fn near_matches<'a>(controls: &'a [Control], query: &str, max: usize) -> Vec<&'a Control> {
    let words: Vec<&str> = query.split(' ').filter(|w| w.len() >= 3).collect();
    let mut scored: Vec<(u8, usize, &Control)> = controls
        .iter()
        .enumerate()
        .filter(|(_, c)| !c.label.is_empty())
        .filter_map(|(i, c)| {
            let label = normalize(&c.label);
            let score = if label.contains(query) || (!label.is_empty() && query.contains(&label)) {
                2
            } else if words.iter().any(|w| label.split(' ').any(|l| l == *w)) {
                1
            } else {
                return None;
            };
            Some((score, i, c))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.into_iter().take(max).map(|(_, _, c)| c).collect()
}

fn resolve<'a>(controls: &'a [Control], target: &Target) -> Result<&'a Control, String> {
    match target {
        Target::Ref(r) => {
            let hits: Vec<&Control> = controls
                .iter()
                .filter(|c| short_ref(c.id) == *r || c.id == *r)
                .collect();
            match hits.as_slice() {
                [one] => Ok(one),
                [] => Err(format!(
                    "Nothing on the screen is #{r} now: it may have closed. `ui_tree` gives the refs \
                     of what is there."
                )),
                many => Err(format!(
                    "#{r} is {} controls at once ({}); press it by its label instead.",
                    many.len(),
                    many.iter().map(|c| mention(c)).collect::<Vec<_>>().join(", ")
                )),
            }
        }
        Target::Label(label) => {
            let query = normalize(label);
            if query.is_empty() {
                return Err(format!("\"{label}\" has no letters or digits to look for."));
            }
            let exact: Vec<&Control> = controls
                .iter()
                .filter(|c| !c.withheld && normalize(&c.label) == query)
                .collect();
            if let Some(best) = exact
                .iter()
                .find(|c| c.enabled && c.tap.is_some())
                .or_else(|| exact.first())
            {
                return Ok(best);
            }
            let near: Vec<String> = near_matches(controls, &query, 8)
                .into_iter()
                .map(mention)
                .collect();
            if !near.is_empty() {
                return Err(format!(
                    "Nothing on the screen is labelled \"{label}\". Near: {}.",
                    near.join(", ")
                ));
            }
            let on: Vec<String> = controls
                .iter()
                .filter(|c| !c.label.is_empty())
                .take(12)
                .map(mention)
                .collect();
            Err(if controls.is_empty() {
                format!("Nothing on the screen is labelled \"{label}\": nothing there takes a tap.")
            } else if on.is_empty() {
                format!(
                    "Nothing on the screen is labelled \"{label}\", and nothing there has a label; \
                     `ui_tree` gives the refs."
                )
            } else {
                format!(
                    "Nothing on the screen is labelled \"{label}\". On it: {}.",
                    on.join(", ")
                )
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Answers
// ---------------------------------------------------------------------------

fn line(c: &Control) -> String {
    let mut s = format!("#{} {}", short_ref(c.id), c.kind);
    if c.withheld {
        s.push_str(" (text withheld)");
    } else if c.label.is_empty() {
        s.push_str(" (no label)");
    } else {
        s.push_str(&format!(" \"{}\"", c.label));
    }
    match (c.selected, has_toggle(c.kind)) {
        (Some(true), true) => s.push_str(" on"),
        (Some(false), true) => s.push_str(" off"),
        (Some(true), false) => s.push_str(" selected"),
        _ => {}
    }
    if let Some(v) = c.value {
        s.push_str(&format!(" = {}", (v * 1000.0).round() / 1000.0));
    }
    if let Some(choice) = &c.choice {
        s.push_str(&format!(" = \"{choice}\""));
    }
    if !c.enabled {
        s.push_str(" disabled");
    }
    if c.tap.is_none() {
        s.push_str(" covered");
    }
    let at = c.rect.center();
    s.push_str(&format!(" at {:.0},{:.0}", at.x, at.y));
    s
}

fn round1(v: f32) -> f64 {
    (f64::from(v) * 10.0).round() / 10.0
}

fn control_json(c: &Control, screen: Rect) -> Value {
    let at = c.rect.center();
    let (w, h) = (screen.width().max(1.0), screen.height().max(1.0));
    let frac = |v: f32, len: f32| (f64::from(v / len) * 1000.0).round() / 1000.0;
    json!({
        "ref": short_ref(c.id),
        "id": c.id.to_string(),
        "kind": c.kind,
        "label": (!c.label.is_empty()).then_some(c.label.as_str()),
        "withheld": c.withheld,
        "enabled": c.enabled,
        "selected": c.selected,
        "value": c.value,
        "choice": c.choice,
        "rect": [round1(c.rect.min.x), round1(c.rect.min.y), round1(c.rect.max.x), round1(c.rect.max.y)],
        "centre": [frac(at.x - screen.min.x, w), frac(at.y - screen.min.y, h)],
        "covered": c.tap.is_none(),
    })
}

fn filtered<'a>(controls: &'a [Control], contains: Option<&str>) -> Vec<&'a Control> {
    let query = contains.map(normalize).filter(|q| !q.is_empty());
    controls
        .iter()
        .filter(|c| match &query {
            None => true,
            Some(q) => !c.withheld && normalize(&c.label).contains(q.as_str()),
        })
        .collect()
}

fn withheld_note(n: usize) -> Option<String> {
    (n > 0).then(|| {
        format!(
            "The text inside {n} control{} is withheld: online, and the allow-list is not everyone.",
            if n == 1 { "" } else { "s" }
        )
    })
}

fn tree_reply(snap: &Snapshot, contains: Option<&str>) -> AgentReply {
    let shown = filtered(&snap.controls, contains);
    let size = format!("{:.0}×{:.0} pt", snap.screen.width(), snap.screen.height());
    let mut text = match (shown.len(), contains) {
        (0, Some(q)) => format!("Nothing on the screen ({size}) has a label containing \"{q}\"."),
        (0, None) => format!("Nothing on the screen ({size}) takes a tap right now."),
        (n, Some(q)) => format!(
            "{n} control{} labelled with \"{q}\" on the screen ({size}):",
            if n == 1 { "" } else { "s" }
        ),
        (n, None) => format!(
            "{n} control{} on the screen ({size}); `press` takes a ref or a label:",
            if n == 1 { "" } else { "s" }
        ),
    };
    for c in shown.iter().take(LIST_MAX) {
        text.push('\n');
        text.push_str(&line(c));
    }
    if shown.len() > LIST_MAX {
        text.push_str(&format!(
            "\n… and {} more; `contains` narrows it.",
            shown.len() - LIST_MAX
        ));
    }
    if let Some(note) = withheld_note(snap.withheld) {
        text.push('\n');
        text.push_str(&note);
    }
    AgentReply::ok(
        text,
        json!({
            "screen": { "width": round1(snap.screen.width()), "height": round1(snap.screen.height()) },
            "controls": shown.iter().map(|c| control_json(c, snap.screen)).collect::<Vec<_>>(),
            "withheld": snap.withheld,
        }),
    )
}

/// Labels in `after` and not in `before`, once each, in `after`'s order.
fn appeared(before: &[Control], after: &[Control], max: usize) -> Vec<String> {
    let old: HashSet<&str> = before.iter().map(|c| c.label.as_str()).collect();
    let mut seen = HashSet::new();
    after
        .iter()
        .map(|c| c.label.as_str())
        .filter(|l| !l.is_empty() && !old.contains(l) && seen.insert(*l))
        .take(max)
        .map(str::to_string)
        .collect()
}

fn quoted(labels: &[String]) -> String {
    labels
        .iter()
        .map(|l| format!("\"{l}\""))
        .collect::<Vec<_>>()
        .join(", ")
}

fn after_reply(plan: &Plan, before: &[Control], snap: &Snapshot) -> AgentReply {
    let what = if plan.control.label.is_empty() {
        format!("{} #{}", plan.control.kind, short_ref(plan.control.id))
    } else {
        format!("{} \"{}\"", plan.control.kind, plan.control.label)
    };
    let mut text = format!(
        "{} {what} at {:.0},{:.0}.",
        capitalised(plan.how()),
        plan.at.x,
        plan.at.y
    );
    let new = appeared(before, &snap.controls, 10);
    let gone = appeared(&snap.controls, before, 10);
    if !new.is_empty() {
        text.push_str(&format!(" Appeared: {}.", quoted(&new)));
    }
    if !gone.is_empty() {
        text.push_str(&format!(" Gone: {}.", quoted(&gone)));
    }
    if new.is_empty() && gone.is_empty() {
        text.push_str(" No control appeared or went.");
    }
    text.push_str(&format!(
        "\nOn the screen now, {} controls:",
        snap.controls.len()
    ));
    for c in snap.controls.iter().take(AFTER_MAX) {
        text.push('\n');
        text.push_str(&line(c));
    }
    if snap.controls.len() > AFTER_MAX {
        text.push_str(&format!(
            "\n… and {} more (`ui_tree`).",
            snap.controls.len() - AFTER_MAX
        ));
    }
    if let Some(note) = withheld_note(snap.withheld) {
        text.push('\n');
        text.push_str(&note);
    }
    AgentReply::ok(
        text,
        json!({
            "pressed": control_json(&plan.control, snap.screen),
            "how": if plan.long { if plan.touch { "long press" } else { "right click" } } else { "tap" },
            "at": [round1(plan.at.x), round1(plan.at.y)],
            "appeared": new,
            "gone": gone,
            "controls": snap.controls.iter().map(|c| control_json(c, snap.screen)).collect::<Vec<_>>(),
        }),
    )
}

fn capitalised(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

// ---------------------------------------------------------------------------
// The system
// ---------------------------------------------------------------------------

enum Ask {
    Tree { contains: Option<String> },
    Press { target: Target, long: bool },
}

/// Frames a press is given to show what it did before the screen is read
/// again: the handler runs in the press's frame (`drive_ui`), the screen's
/// system takes the tap in the next, the state it changes is drawn and laid
/// out in the one after.
#[cfg(feature = "ui-probe")]
const SETTLE_FRAMES: u64 = 3;

#[cfg(feature = "ui-probe")]
enum Stage {
    Look,
    Pressed {
        plan: Plan,
        before: Vec<Control>,
        frame: u64,
    },
}

#[cfg(feature = "ui-probe")]
struct Job {
    label: &'static str,
    ask: Ask,
    reply: oneshot::Sender<Reply>,
    stage: Stage,
    /// When the current stage began (`Time<Real>`).
    since: f64,
}

/// The screen calls in flight, oldest first. One is worked at a time — a press
/// is input, and two at once would be one gesture.
#[derive(Resource, Default)]
pub struct ScreenPending {
    #[cfg(feature = "ui-probe")]
    jobs: VecDeque<Job>,
    /// Frames [`serve`] has seen, for a press's settling.
    #[cfg(feature = "ui-probe")]
    frames: u64,
}

impl ScreenPending {
    /// Something here keeps the frame rate up (`mcp::pace_idle`).
    pub fn is_working(&self) -> bool {
        #[cfg(feature = "ui-probe")]
        let working = !self.jobs.is_empty();
        #[cfg(not(feature = "ui-probe"))]
        let working = false;
        working
    }
}

/// The calls the pump queued, as asks, each noted as working; one that
/// cannot be asked is answered here.
fn take_asks(bridge: &mut McpBridge, now: f64) -> Vec<(&'static str, Ask, oneshot::Sender<Reply>)> {
    let mut out = Vec::new();
    for (request, reply) in std::mem::take(&mut bridge.screen) {
        let label = request.label();
        let ask = match request {
            DeviceRequest::UiTree(UiTreeArgs { contains }) => Ask::Tree { contains },
            DeviceRequest::Press(args) => match target_of(&args) {
                Ok(target) => Ask::Press {
                    target,
                    long: args.long.unwrap_or(false),
                },
                Err(why) => {
                    answer(bridge, label, reply, AgentReply::refuse(why), now);
                    continue;
                }
            },
            // The pump routes only the screen's two here.
            _ => {
                answer(
                    bridge,
                    label,
                    reply,
                    AgentReply::refuse("That is not a question about the screen."),
                    now,
                );
                continue;
            }
        };
        bridge.note(label.into(), "working…".into(), Outcome::Working, now);
        out.push((label, ask, reply));
    }
    out
}

/// Answer `ui_tree` and `press`, which the pump queues in `McpBridge::screen`,
/// from the component library's tree. Exclusive, on the main thread, because
/// the library's host is thread-local and a press triggers pointer events in
/// the world.
///
/// The library's tree is always there to read: a `ui_tree` is answered in the
/// frame it arrives. A press triggers the pointer's down, up and click on what
/// its point hits (`mithril_bevy::press`), so a covered control is not
/// reached; the screen is read again [`SETTLE_FRAMES`] later.
#[cfg(feature = "ui-probe")]
pub fn serve(world: &mut World) {
    let now = world.resource::<Time<Real>>().elapsed_secs_f64();
    let hide_text =
        world.resource::<SessionState>().is_online() && !world.resource::<Audience>().everyone;
    world.resource_scope(|world, mut pending: Mut<ScreenPending>| {
        pending.frames += 1;
        let asks = take_asks(&mut world.resource_mut::<McpBridge>(), now);
        for (label, ask, reply) in asks {
            pending.jobs.push_back(Job {
                label,
                ask,
                reply,
                stage: Stage::Look,
                since: now,
            });
        }
        let frame = pending.frames;
        let Some(job) = pending.jobs.front_mut() else {
            return;
        };
        if let Some(answered) = step(world, job, now, frame, hide_text) {
            if let Some(job) = pending.jobs.pop_front() {
                answer(
                    &mut world.resource_mut::<McpBridge>(),
                    job.label,
                    job.reply,
                    answered,
                    now,
                );
            }
            // The next call's clock starts now, not when it was queued.
            if let Some(next) = pending.jobs.front_mut() {
                next.since = now;
            }
        }
    });
}

/// Built without the component library's bevy_ui backend there is no tree
/// to read: every call is refused at once.
#[cfg(not(feature = "ui-probe"))]
pub fn serve(mut bridge: ResMut<McpBridge>, real: Res<Time<Real>>) {
    let now = real.elapsed_secs_f64();
    for (label, _, reply) in take_asks(&mut bridge, now) {
        answer(
            &mut bridge,
            label,
            reply,
            AgentReply::refuse(
                "This build has no interface the screen tools can read (built without `ui-probe`).",
            ),
            now,
        );
    }
}

/// Move one call on; its answer once it has one.
#[cfg(feature = "ui-probe")]
fn step(
    world: &mut World,
    job: &mut Job,
    now: f64,
    frame: u64,
    hide_text: bool,
) -> Option<AgentReply> {
    if now - job.since > STAGE_TIMEOUT_S {
        return Some(AgentReply::refuse(format!(
            "The screen drew no new frame for {STAGE_TIMEOUT_S:.0} s, so there is nothing to \
             read; the app may be in the background."
        )));
    }
    match std::mem::replace(&mut job.stage, Stage::Look) {
        Stage::Look => {
            // No root: the library is not drawing yet; wait.
            let snap = lib_snapshot(world, hide_text)?;
            let (target, long) = match &job.ask {
                Ask::Tree { contains } => return Some(tree_reply(&snap, contains.as_deref())),
                Ask::Press { target, long } => (target, *long),
            };
            let control = match resolve(&snap.controls, target) {
                Ok(c) => c.clone(),
                Err(why) => return Some(AgentReply::refuse(why)),
            };
            let name = mention(&control);
            if long {
                return Some(AgentReply::refuse(format!(
                    "{name} is drawn by the component library, which has no long press yet; \
                     press it without `long`."
                )));
            }
            if !control.enabled {
                return Some(AgentReply::refuse(format!(
                    "{name} is disabled: pressing it would do nothing."
                )));
            }
            let Some(at) = control.tap else {
                return Some(AgentReply::refuse(format!(
                    "{name} is covered by another window or sheet everywhere a finger \
                     could land; close that first."
                )));
            };
            let node = mithril_bevy::NodeId(control.handle);
            match mithril_bevy::press(world, node, at) {
                Ok(()) => {}
                Err(mithril_bevy::PressError::Gone) => {
                    return Some(AgentReply::refuse(format!(
                        "{name} closed before it could be pressed; `ui_tree` gives what is there."
                    )))
                }
                Err(mithril_bevy::PressError::Covered) => {
                    return Some(AgentReply::refuse(format!(
                        "{name} is covered by another window or sheet where a finger would \
                         land; close that first."
                    )))
                }
            }
            job.stage = Stage::Pressed {
                plan: Plan {
                    control,
                    at,
                    touch: true,
                    long: false,
                },
                before: snap.controls,
                frame,
            };
            job.since = now;
            None
        }
        Stage::Pressed {
            plan,
            before,
            frame: pressed,
        } => {
            let snap = (frame >= pressed + SETTLE_FRAMES)
                .then(|| lib_snapshot(world, hide_text))
                .flatten();
            match snap {
                Some(snap) => Some(after_reply(&plan, &before, &snap)),
                None => {
                    job.stage = Stage::Pressed {
                        plan,
                        before,
                        frame: pressed,
                    };
                    None
                }
            }
        }
    }
}

fn answer(
    bridge: &mut McpBridge,
    label: &'static str,
    reply: oneshot::Sender<Reply>,
    body: AgentReply,
    now: f64,
) {
    let outcome = if body.error {
        Outcome::Failed
    } else {
        Outcome::Done
    };
    let first = body.text.lines().next().unwrap_or_default().to_string();
    bridge.note(label.into(), first, outcome, now);
    let _ = reply.send(Reply::Agent(Box::new(body)));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn control(id: u64, kind: &'static str, label: &str) -> Control {
        Control {
            id,
            handle: id,
            kind,
            label: label.to_string(),
            withheld: false,
            enabled: true,
            selected: None,
            value: None,
            choice: None,
            rect: Rect::new(10.0, 10.0 * id as f32, 90.0, 10.0 * id as f32 + 24.0),
            tap: Some(Vec2::new(50.0, 10.0 * id as f32 + 12.0)),
        }
    }

    #[test]
    fn labels_match_without_icons_case_or_punctuation() {
        assert_eq!(normalize("⚙  Settings…"), "settings");
        assert_eq!(normalize("Sign IN"), normalize("sign in"));
        assert_eq!(normalize("🔍"), "");
    }

    #[test]
    fn a_label_presses_the_first_reachable_match() {
        let mut covered = control(1, "button", "Close");
        covered.tap = None;
        let reachable = control(2, "button", "✕ close");
        let controls = vec![covered, reachable, control(3, "button", "Close all")];
        let hit = resolve(&controls, &Target::Label("CLOSE".into())).unwrap();
        assert_eq!(hit.id, 2);
    }

    #[test]
    fn a_missing_label_names_the_near_ones() {
        let controls = vec![
            control(1, "button", "Open settings"),
            control(2, "button", "Search"),
        ];
        let why = resolve(&controls, &Target::Label("settings".into())).unwrap_err();
        assert!(why.contains("Open settings"), "{why}");
        assert!(!why.contains("Search"), "{why}");
        let why = resolve(&controls, &Target::Label("zebra".into())).unwrap_err();
        assert!(
            why.contains("Search") && why.contains("Open settings"),
            "{why}"
        );
    }

    #[test]
    fn a_ref_is_the_short_one_or_the_whole_id() {
        let id = 0xDEAD_BEEF_1234_5678;
        let controls = vec![control(id, "button", "A"), control(7, "button", "B")];
        assert_eq!(
            resolve(&controls, &Target::Ref(short_ref(id)))
                .unwrap()
                .label,
            "A"
        );
        assert_eq!(resolve(&controls, &Target::Ref(id)).unwrap().label, "A");
        assert!(resolve(&controls, &Target::Ref(999_999_999)).is_err());
        assert!(short_ref(u64::MAX) < 1_000_000);
    }

    #[test]
    fn two_controls_on_one_ref_are_refused_not_guessed() {
        let a = 5;
        let b = 5 + 1_000_000; // folds to the same six digits
        assert_eq!(short_ref(a), short_ref(b));
        let controls = vec![control(a, "button", "A"), control(b, "button", "B")];
        let why = resolve(&controls, &Target::Ref(short_ref(a))).unwrap_err();
        assert!(why.contains("label"), "{why}");
    }

    #[test]
    fn contains_filters_and_withheld_text_never_matches() {
        let mut row = control(3, "clickable area", "");
        row.withheld = true;
        let controls = vec![
            control(1, "button", "Show buildings"),
            control(2, "checkbox", "Show trees"),
            row,
        ];
        assert_eq!(filtered(&controls, Some("show")).len(), 2);
        assert_eq!(filtered(&controls, Some("TREES")).len(), 1);
        assert_eq!(filtered(&controls, None).len(), 3);
        assert_eq!(filtered(&controls, Some("  ")).len(), 3);
    }

    #[test]
    fn a_line_says_state_and_never_leaks_a_withheld_label() {
        let mut tick = control(1, "checkbox", "Show trees");
        tick.selected = Some(true);
        tick.enabled = false;
        let l = line(&tick);
        assert!(
            l.contains("\"Show trees\" on") && l.contains("disabled"),
            "{l}"
        );
        let mut row = control(2, "clickable area", "");
        row.withheld = true;
        row.tap = None;
        let l = line(&row);
        assert!(
            l.contains("(text withheld)") && l.contains("covered"),
            "{l}"
        );
    }

    #[test]
    fn long_labels_are_clipped_to_one_line() {
        let long = "word ".repeat(40);
        let out = clip(&long);
        assert_eq!(out.chars().count(), LABEL_MAX);
        assert!(out.ends_with('…'));
        assert_eq!(clip("two\nlines"), "two lines");
    }

    #[test]
    fn what_appeared_is_counted_once() {
        let before = vec![control(1, "button", "Menu")];
        let after = vec![
            control(1, "button", "Menu"),
            control(2, "button", "Settings"),
            control(3, "button", "Settings"),
        ];
        assert_eq!(appeared(&before, &after, 10), vec!["Settings".to_string()]);
        assert!(appeared(&after, &before, 10).is_empty());
    }

    // --- Through the component library's tree ----------------------------

    /// A library screen mounted in a world of its own, laid out in rows
    /// (`mithril_bevy::testing`), two physical px a point.
    #[cfg(feature = "ui-probe")]
    fn library(view: mithril::view::View) -> World {
        let mut world = mithril_bevy::testing::world();
        mithril_bevy::with_host(|h| h.mount(view));
        mithril_bevy::drive_ui(&mut world);
        mithril_bevy::testing::lay_out_rows(&mut world, 2.0);
        world
    }

    #[cfg(feature = "ui-probe")]
    #[test]
    fn the_library_tree_lists_a_screens_buttons_by_label() {
        let world = library(crate::systems::lib_reminder::demo_view());
        let snap = lib_snapshot(&world, true).expect("the library has a root");
        let reply = tree_reply(&snap, None);
        let labels: Vec<&str> = snap.controls.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(
            labels,
            ["Close", "Open chat", "Walk there"],
            "{}",
            reply.text
        );
        assert!(
            snap.controls
                .iter()
                .all(|c| c.kind == "button" && c.enabled),
            "{}",
            reply.text
        );
        // The card listens for clicks too, but holds the buttons: a container.
        assert!(!reply.text.contains("clickable area"), "{}", reply.text);
        assert!(
            reply.text.contains("button \"Walk there\""),
            "{}",
            reply.text
        );
        // Rects in points: a row is 200 × 32 physical px.
        let walk = &snap.controls[2];
        assert_eq!((walk.rect.width(), walk.rect.height()), (100.0, 16.0));
        assert_eq!(walk.tap, Some(walk.rect.center()));
        // Six digits folded from the id.
        let r = reply.data["controls"][2]["ref"].as_u64().unwrap();
        assert_eq!(r, short_ref(walk.id));
        assert_eq!(
            resolve(&snap.controls, &Target::Ref(r)).unwrap().label,
            "Walk there"
        );
    }

    #[cfg(feature = "ui-probe")]
    #[test]
    fn a_library_press_runs_the_buttons_handler() {
        use mithril::prelude::{Button, Component as _, Event};
        use std::cell::Cell;
        use std::rc::Rc;
        let saves = Rc::new(Cell::new(0));
        let s = saves.clone();
        let view = mithril::view::el("div")
            .child(
                Button {
                    onclick: Some(Rc::new(move |_: &Event| s.set(s.get() + 1))),
                    children: "Save".into(),
                    ..Default::default()
                }
                .render(),
            )
            .child(
                Button {
                    disabled: true.into(),
                    children: "Delete".into(),
                    ..Default::default()
                }
                .render(),
            );
        let mut world = library(mithril::view::IntoView::into_view(view));
        let snap = lib_snapshot(&world, false).unwrap();
        let save = resolve(&snap.controls, &Target::Label("save".into()))
            .unwrap()
            .clone();
        let at = save.tap.unwrap();
        mithril_bevy::press(&mut world, mithril_bevy::NodeId(save.handle), at).unwrap();
        mithril_bevy::drive_ui(&mut world);
        assert_eq!(saves.get(), 1);
        let delete = resolve(&snap.controls, &Target::Label("delete".into())).unwrap();
        assert!(!delete.enabled);
    }
}
