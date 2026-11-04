//! The two tools about the screen itself: `ui_tree` lists the controls egui drew
//! on the last pass, and `press` taps one the way a finger would.
//!
//! **The labels come from egui's AccessKit tree, switched on only while a call
//! is waiting.** egui keeps no label with a widget's rect outside debug builds
//! (`WidgetRects` has ids, rects and senses; its `WidgetInfo` store is
//! `debug_assertions`-only and gated on a debug style flag). What does carry
//! role, label, hint and toggle state is the AccessKit node every widget fills
//! when `Context::enable_accesskit` is on — built in the pass, handed out in
//! `PlatformOutput::accesskit_update`, which this client's `end_egui_frame`
//! never reads. So nothing here touches the egui bridge: an egui `Plugin`
//! ([`ScreenHook`]) reads the tree in its `output_hook`, right after `end_pass`,
//! when `prev_pass.widgets` is the pass that tree describes, and joins the two by
//! `Id`. The rects come from `WidgetRects` (clipped `interact_rect`, moved to
//! global points by the layer's transform), not from the nodes' bounds.
//!
//! AccessKit state is created at `begin_pass`, so switching it on mid-frame
//! yields a tree one pass later: a `ui_tree` is answered two frames after it
//! arrives, and it is switched off again once the queue is empty, so an idle
//! client pays nothing.
//!
//! **A press is input, not an AccessKit action.** egui does honour
//! `ActionRequest { action: Click }`, but that skips the hit test, so it would
//! press a button under a sheet that a finger could not reach. Instead the same
//! hook's `input_hook` appends events to the next `RawInput`, the sequence
//! `egui_manager::pointer_events` makes for a finger (`Touch` start, move,
//! press; then `Touch` end, release, `PointerGone`), on a frame of its own each.
//! On a screen egui has never seen a touch on (a desktop), the touch events are
//! left out — one `Event::Touch` makes egui's `has_touch_screen` true for good,
//! which turns off drag-to-select in every label — and a long press, which egui
//! only recognises from a touch, becomes the right click that opens the same
//! context menus (`Response::context_menu` is either).
//!
//! **Text is withheld here as it is from a picture.** A text field's contents
//! never leave: only its hint or the label it is `labelled_by`. Plain labels are
//! not controls and are not listed (egui makes every label selectable, so they
//! all sense a click). A clickable row with no label of its own is named by the
//! text inside it — and that text is left out while online with an allow-list
//! narrower than everyone, the rule `pump` applies to the chrome in a `view`,
//! because a row in the chat panel is somebody's words.

use std::collections::{HashMap, HashSet, VecDeque};

use bevy::prelude::*;
use egui::accesskit::{self, Action, Role, Toggled};
use futures_channel::oneshot;
use serde_json::{json, Value};

use super::agent::Audience;
use super::{DeviceRequest, McpBridge, Outcome, Reply};
use crate::systems::egui_manager::EguiRef;
use crate::systems::session::SessionState;
use cartopolis_mcp::args::AgentReply;
use cartopolis_mcp::device::{PressArgs, UiTreeArgs};

/// How long one step may wait for the UI to draw before the call is refused. A
/// headless client idles at two frames a second (`mcp::pace_idle`), and a press
/// is several frames.
const STAGE_TIMEOUT_S: f64 = 15.0;
/// Held past egui's own long-press threshold (`max_click_duration`) by this much.
const LONG_MARGIN_S: f64 = 0.2;
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
/// The finger's id. Real fingers are numbered from zero by the platform.
const FINGER: egui::TouchId = egui::TouchId(u64::MAX - 7);

// ---------------------------------------------------------------------------
// What is on the screen
// ---------------------------------------------------------------------------

/// One control egui drew, in global egui points.
#[derive(Clone, Debug, PartialEq)]
pub struct Control {
    /// egui's `Id` value. Stable while the widget's id path is.
    pub id: u64,
    pub kind: &'static str,
    /// Empty when it has none (or when it is [`Self::withheld`]).
    pub label: String,
    /// Named only by text inside it that is not shown here.
    pub withheld: bool,
    pub enabled: bool,
    /// A checkbox's tick, a selectable's selection.
    pub selected: Option<bool>,
    /// A slider's or number field's value.
    pub value: Option<f64>,
    pub rect: egui::Rect,
    /// Where a finger lands on it: its centre, or another point of it when the
    /// centre is under another layer. `None` when all of it is covered.
    pub tap: Option<egui::Pos2>,
}

/// Every control of one pass, in reading order.
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub screen: egui::Rect,
    pub controls: Vec<Control>,
    pub withheld: usize,
}

/// The kind a role is shown as, or `None` for one that is not a control.
fn kind_of(role: Role) -> Option<&'static str> {
    Some(match role {
        Role::Button => "button",
        Role::CheckBox => "checkbox",
        Role::Switch => "switch",
        Role::RadioButton => "radio button",
        Role::Tab => "tab",
        Role::TextInput | Role::MultilineTextInput | Role::PasswordInput => "text field",
        Role::Slider => "slider",
        Role::SpinButton => "number field",
        Role::ComboBox => "dropdown",
        Role::Link => "link",
        Role::ColorWell => "colour picker",
        Role::Image => "image",
        Role::MenuItem => "menu item",
        Role::ListBoxOption => "option",
        // A `UiBuilder::sense` scope (a whole row that is one button), or a
        // widget that never said what it is.
        Role::GenericContainer | Role::Unknown => "clickable area",
        _ => return None,
    })
}

fn is_text_field(role: Role) -> bool {
    matches!(
        role,
        Role::TextInput | Role::MultilineTextInput | Role::PasswordInput
    )
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

fn name_of(
    node: &accesskit::Node,
    nodes: &HashMap<u64, &accesskit::Node>,
    hide_text: bool,
) -> Name {
    // A field's `labelled_by`, a control's own label. Never a text field's
    // value — that is what somebody typed.
    let by: Vec<&str> = node
        .labelled_by()
        .iter()
        .filter_map(|id| nodes.get(&id.0))
        .filter(|n| !is_text_field(n.role()))
        .filter_map(|n| n.label().or_else(|| n.value()))
        .filter(|s| !s.trim().is_empty())
        .collect();
    if !by.is_empty() {
        return Name::Own(by.join(" "));
    }
    if is_text_field(node.role()) {
        return match node.placeholder().filter(|s| !s.trim().is_empty()) {
            Some(hint) => Name::Own(hint.to_string()),
            None => Name::Nothing,
        };
    }
    if let Some(label) = node.label().filter(|s| !s.trim().is_empty()) {
        return Name::Own(label.to_string());
    }
    match inner_text(node, nodes) {
        None => Name::Container,
        Some(text) if text.is_empty() => Name::Nothing,
        Some(_) if hide_text => Name::Withheld,
        Some(text) => Name::Inner(text.join(" ")),
    }
}

/// The plain labels inside a node, in order — or `None` when it holds another
/// control or more text than a row's worth.
fn inner_text<'a>(
    root: &accesskit::Node,
    nodes: &HashMap<u64, &'a accesskit::Node>,
) -> Option<Vec<&'a str>> {
    const VISITED_MAX: usize = 64;
    let mut out = Vec::new();
    let mut stack: Vec<u64> = root.children().iter().rev().map(|c| c.0).collect();
    let mut visited = 0;
    while let Some(id) = stack.pop() {
        visited += 1;
        if visited > VISITED_MAX {
            return None;
        }
        let Some(&node) = nodes.get(&id) else {
            continue;
        };
        let role = node.role();
        if role == Role::Label {
            // Every label senses a click (they are selectable), so a label's
            // Click action says nothing; its text is the row's name.
            if let Some(text) = node.value().filter(|s| !s.trim().is_empty()) {
                out.push(text);
                if out.len() > ROW_TEXT_MAX {
                    return None;
                }
            }
            continue;
        }
        if is_text_field(role)
            || node.supports_action(Action::Click)
            || node.supports_action(Action::Focus)
        {
            return None;
        }
        stack.extend(node.children().iter().rev().map(|c| c.0));
    }
    Some(out)
}

/// The first of `spots` inside `rect` where `on_top` says this control's layer
/// is the one a finger would reach: the centre, then the middles of its four
/// sides, then its quarters.
fn tap_point(rect: egui::Rect, mut on_top: impl FnMut(egui::Pos2) -> bool) -> Option<egui::Pos2> {
    const SPOTS: [(f32, f32); 9] = [
        (0.5, 0.5),
        (0.25, 0.5),
        (0.75, 0.5),
        (0.5, 0.25),
        (0.5, 0.75),
        (0.25, 0.25),
        (0.75, 0.25),
        (0.25, 0.75),
        (0.75, 0.75),
    ];
    SPOTS
        .iter()
        .map(|&(x, y)| rect.lerp_inside(egui::vec2(x, y)))
        .find(|&p| on_top(p))
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

/// Build the snapshot of the pass that just ended, from its AccessKit tree and
/// its widget rects. Runs in [`ScreenHook::output_hook`].
fn snapshot(ctx: &egui::Context, update: &accesskit::TreeUpdate, hide_text: bool) -> Snapshot {
    let nodes: HashMap<u64, &accesskit::Node> =
        update.nodes.iter().map(|(id, node)| (id.0, node)).collect();
    // `end_pass` has swapped the passes, so `prev_pass` is the one just drawn.
    let widgets: Vec<(egui::LayerId, egui::WidgetRect)> =
        ctx.viewport_for(egui::ViewportId::ROOT, |v| {
            v.prev_pass
                .widgets
                .layers()
                .flat_map(|(layer, rects)| rects.iter().map(move |w| (*layer, *w)))
                .collect()
        });
    let screen = ctx.viewport_rect();
    let mut seen = HashSet::new();
    let mut out = Snapshot {
        screen,
        controls: Vec::new(),
        withheld: 0,
    };
    for (layer, w) in widgets {
        if !(w.sense.senses_click() || w.sense.is_focusable()) || !seen.insert(w.id) {
            continue;
        }
        let node = nodes.get(&w.id.value()).copied();
        let role = node.map_or(Role::Unknown, |n| n.role());
        let Some(kind) = kind_of(role) else {
            continue;
        };
        let to_global = ctx
            .layer_transform_to_global(layer)
            .unwrap_or(egui::emath::TSTransform::IDENTITY);
        let rect = (to_global * w.interact_rect).intersect(screen);
        if !rect.is_positive() {
            continue;
        }
        let name = node.map_or(Name::Nothing, |n| name_of(n, &nodes, hide_text));
        let (label, withheld) = match name {
            Name::Container => continue,
            Name::Nothing if rect.area() > NAMELESS_MAX_SHARE * screen.area() => continue,
            Name::Nothing => (String::new(), false),
            Name::Own(text) | Name::Inner(text) => (clip(&text), false),
            Name::Withheld => (String::new(), true),
        };
        out.withheld += usize::from(withheld);
        let selected = node
            .and_then(|n| n.toggled())
            .map(|t| matches!(t, Toggled::True));
        let tap = tap_point(rect, |p| match ctx.layer_id_at(p) {
            None => true,
            Some(top) => top == layer,
        });
        out.controls.push(Control {
            id: w.id.value(),
            kind,
            label,
            withheld,
            enabled: w.enabled && !node.is_some_and(|n| n.is_disabled()),
            selected,
            value: node.and_then(|n| n.numeric_value()),
            rect,
            tap,
        });
    }
    drop_shadows(&mut out.controls);
    reading_order(&mut out.controls);
    out
}

/// Drop a nameless control lying inside a named one: the `Area` or frame a
/// map button stands in senses the same click, and listed it twice over, once
/// with the button's name and once as an unlabelled area at the same place.
fn drop_shadows(controls: &mut Vec<Control>) {
    let named: Vec<egui::Rect> = controls
        .iter()
        .filter(|c| !c.label.is_empty() || c.withheld)
        .map(|c| c.rect)
        .collect();
    controls.retain(|c| {
        !c.label.is_empty()
            || c.withheld
            || !named.iter().any(|r| r.expand(1.0).contains_rect(c.rect))
    });
}

// ---------------------------------------------------------------------------
// The egui side
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Want {
    /// The first pass whose tree will do.
    from_pass: u64,
    hide_text: bool,
}

/// Sits in egui's pass: counts passes, feeds the next pass the events a press
/// queued, and reads the AccessKit tree when one is wanted. See the module note.
#[derive(Default)]
struct ScreenHook {
    passes: u64,
    want: Option<Want>,
    taken: Option<Snapshot>,
    inject: Vec<egui::Event>,
}

impl egui::plugin::Plugin for ScreenHook {
    fn debug_name(&self) -> &'static str {
        "cartopolis mcp screen"
    }

    fn input_hook(&mut self, _ctx: &egui::Context, input: &mut egui::RawInput) {
        self.passes += 1;
        // After the frame's own events, where `inject_events` puts the on-screen
        // keyboard's — a press is a frame of its own and carries its position.
        input.events.append(&mut self.inject);
    }

    fn output_hook(&mut self, ctx: &egui::Context, output: &mut egui::FullOutput) {
        let Some(want) = self.want else {
            return;
        };
        if self.passes < want.from_pass {
            return;
        }
        // `None` on the pass AccessKit was switched on in: wait for the next.
        let Some(update) = output.platform_output.accesskit_update.as_ref() else {
            return;
        };
        self.taken = Some(snapshot(ctx, update, want.hide_text));
        self.want = None;
    }
}

fn with_hook<R>(ctx: &egui::Context, f: impl FnOnce(&mut ScreenHook) -> R) -> Option<R> {
    let handle = ctx.plugin_opt::<ScreenHook>()?;
    let mut hook = handle.lock();
    Some(f(&mut hook))
}

/// Ask for the tree of a pass at least `ahead` passes after the current one.
fn ask_snapshot(ctx: &egui::Context, ahead: u64, hide_text: bool) {
    with_hook(ctx, |h| {
        h.taken = None;
        h.want = Some(Want {
            from_pass: h.passes + ahead,
            hide_text,
        });
    });
}

/// How a press is made.
#[derive(Clone, Debug)]
struct Plan {
    control: Control,
    at: egui::Pos2,
    /// As a finger (with `Event::Touch`), or as a mouse.
    touch: bool,
    long: bool,
}

impl Plan {
    /// A mouse has no long press; the right button opens the same menus.
    fn button(&self) -> egui::PointerButton {
        if self.long && !self.touch {
            egui::PointerButton::Secondary
        } else {
            egui::PointerButton::Primary
        }
    }

    fn how(&self) -> &'static str {
        match (self.long, self.touch) {
            (false, _) => "tapped",
            (true, true) => "long-pressed",
            (true, false) => "right-clicked (no touch screen here, so that is the long press)",
        }
    }
}

fn down_events(plan: &Plan) -> Vec<egui::Event> {
    let mut out = Vec::new();
    if plan.touch {
        out.push(egui::Event::Touch {
            device_id: egui::TouchDeviceId(0),
            id: FINGER,
            phase: egui::TouchPhase::Start,
            pos: plan.at,
            force: None,
        });
    }
    // Move first: egui hit-tests the press where the pointer is.
    out.push(egui::Event::PointerMoved(plan.at));
    out.push(egui::Event::PointerButton {
        pos: plan.at,
        button: plan.button(),
        pressed: true,
        modifiers: egui::Modifiers::default(),
    });
    out
}

fn up_events(plan: &Plan) -> Vec<egui::Event> {
    let mut out = Vec::new();
    if plan.touch {
        out.push(egui::Event::Touch {
            device_id: egui::TouchDeviceId(0),
            id: FINGER,
            phase: egui::TouchPhase::End,
            pos: plan.at,
            force: None,
        });
    }
    out.push(egui::Event::PointerButton {
        pos: plan.at,
        button: plan.button(),
        pressed: false,
        modifiers: egui::Modifiers::default(),
    });
    // Or egui goes on hovering what the finger left.
    out.push(egui::Event::PointerGone);
    out
}

// ---------------------------------------------------------------------------
// Finding a control
// ---------------------------------------------------------------------------

/// The short number a control goes by in `ui_tree`: six digits folded from its
/// egui id, because a model copies 6 digits more reliably than 20. A clash is
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

fn control_json(c: &Control, screen: egui::Rect) -> Value {
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

enum Stage {
    /// Not started: ask for a tree.
    Look,
    /// Waiting for the tree; `after` once the press has been made.
    Asked { after: Option<(Plan, Vec<Control>)> },
    /// The finger is down.
    Held {
        plan: Plan,
        /// The pass in progress when the press was queued; the next one takes it.
        down_pass: u64,
        /// egui's clock on the pass that took the press.
        pressed_at: Option<f64>,
        before: Vec<Control>,
    },
}

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
    jobs: VecDeque<Job>,
    hooked: bool,
    accesskit: bool,
}

impl ScreenPending {
    /// Something here keeps the frame rate up (`mcp::pace_idle`).
    pub fn is_working(&self) -> bool {
        !self.jobs.is_empty()
    }
}

/// Answer `ui_tree` and `press`, which the pump queues in `McpBridge::screen`.
/// Reaches egui only through [`ScreenHook`], an egui plugin it installs on the
/// first call, so it needs [`EguiRef`] and no place in the panel chain.
pub fn serve(
    mut bridge: ResMut<McpBridge>,
    mut pending: ResMut<ScreenPending>,
    egui: EguiRef,
    session: Res<SessionState>,
    audience: Res<Audience>,
    real: Res<Time<Real>>,
) {
    let now = real.elapsed_secs_f64();
    let hide_text = session.is_online() && !audience.everyone;
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
                    answer(&mut bridge, label, reply, AgentReply::refuse(why), now);
                    continue;
                }
            },
            // The pump routes only the screen's two here.
            _ => {
                answer(
                    &mut bridge,
                    label,
                    reply,
                    AgentReply::refuse("That is not a question about the screen."),
                    now,
                );
                continue;
            }
        };
        bridge.note(label.into(), "working…".into(), Outcome::Working, now);
        pending.jobs.push_back(Job {
            label,
            ask,
            reply,
            stage: Stage::Look,
            since: now,
        });
    }

    let ctx = egui.ctx();
    if pending.jobs.is_empty() {
        if pending.accesskit {
            ctx.disable_accesskit();
            with_hook(ctx, |h| h.want = None);
            pending.accesskit = false;
        }
        return;
    }
    if !pending.hooked {
        ctx.add_plugin(ScreenHook::default());
        pending.hooked = true;
    }
    if !pending.accesskit {
        ctx.enable_accesskit();
        pending.accesskit = true;
    }
    let Some(job) = pending.jobs.front_mut() else {
        return;
    };
    if let Some(answered) = step(job, ctx, now, hide_text) {
        if let Some(job) = pending.jobs.pop_front() {
            answer(&mut bridge, job.label, job.reply, answered, now);
        }
        // The next call's clock starts now, not when it was queued.
        if let Some(next) = pending.jobs.front_mut() {
            next.since = now;
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

/// Move one call on by what this frame allows; its answer once it has one.
fn step(job: &mut Job, ctx: &egui::Context, now: f64, hide_text: bool) -> Option<AgentReply> {
    let stage = std::mem::replace(&mut job.stage, Stage::Look);
    if now - job.since > STAGE_TIMEOUT_S {
        // Never leave a finger on the glass.
        if let Stage::Held { plan, .. } = &stage {
            with_hook(ctx, |h| h.inject.extend(up_events(plan)));
        }
        return Some(AgentReply::refuse(format!(
            "The screen drew no new frame for {STAGE_TIMEOUT_S:.0} s, so there is nothing to \
             read; the app may be in the background."
        )));
    }
    match stage {
        Stage::Look => {
            // AccessKit was off for the pass in progress, so the next one.
            ask_snapshot(ctx, 1, hide_text);
            job.stage = Stage::Asked { after: None };
            job.since = now;
            None
        }
        Stage::Asked { after } => {
            let Some(snap) = with_hook(ctx, |h| h.taken.take()).flatten() else {
                job.stage = Stage::Asked { after };
                return None;
            };
            match (&job.ask, after) {
                (Ask::Tree { contains }, _) => Some(tree_reply(&snap, contains.as_deref())),
                (Ask::Press { .. }, Some((plan, before))) => {
                    Some(after_reply(&plan, &before, &snap))
                }
                (Ask::Press { target, long }, None) => {
                    let control = match resolve(&snap.controls, target) {
                        Ok(c) => c.clone(),
                        Err(why) => return Some(AgentReply::refuse(why)),
                    };
                    let name = mention(&control);
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
                    // A phone may not have been touched yet this run.
                    let touch = cfg!(target_os = "android") || ctx.input(|i| i.has_touch_screen());
                    let plan = Plan {
                        control,
                        at,
                        touch,
                        long: *long,
                    };
                    let down_pass = with_hook(ctx, |h| {
                        h.inject.extend(down_events(&plan));
                        h.passes
                    })
                    .unwrap_or_default();
                    job.stage = Stage::Held {
                        plan,
                        down_pass,
                        pressed_at: None,
                        before: snap.controls,
                    };
                    job.since = now;
                    None
                }
            }
        }
        Stage::Held {
            plan,
            down_pass,
            pressed_at,
            before,
        } => {
            // No hook means no pass has been counted: wait, finger and all.
            let passes = with_hook(ctx, |h| h.passes).unwrap_or_default();
            if passes <= down_pass {
                job.stage = Stage::Held {
                    plan,
                    down_pass,
                    pressed_at,
                    before,
                };
                return None;
            }
            let time = ctx.input(|i| i.time);
            let pressed = pressed_at.unwrap_or(time);
            if plan.long && plan.touch {
                let hold = ctx.options(|o| o.input_options.max_click_duration) + LONG_MARGIN_S;
                if time - pressed < hold {
                    job.stage = Stage::Held {
                        plan,
                        down_pass,
                        pressed_at: Some(pressed),
                        before,
                    };
                    return None;
                }
            }
            // The release goes into the next pass, the UI answers it in that
            // pass or the one after, and the tree is read from that one.
            with_hook(ctx, |h| h.inject.extend(up_events(&plan)));
            ask_snapshot(ctx, 2, hide_text);
            job.stage = Stage::Asked {
                after: Some((plan, before)),
            };
            job.since = now;
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::systems::egui_manager::UnpaintedPasses as _;

    fn control(id: u64, kind: &'static str, label: &str) -> Control {
        Control {
            id,
            kind,
            label: label.to_string(),
            withheld: false,
            enabled: true,
            selected: None,
            value: None,
            rect: egui::Rect::from_min_size(
                egui::pos2(10.0, 10.0 * id as f32),
                egui::vec2(80.0, 24.0),
            ),
            tap: Some(egui::pos2(50.0, 10.0 * id as f32 + 12.0)),
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
    fn a_covered_centre_taps_elsewhere_or_nowhere() {
        let rect = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(100.0, 20.0));
        assert_eq!(tap_point(rect, |_| true), Some(egui::pos2(50.0, 10.0)));
        let left = tap_point(rect, |p| p.x < 40.0).unwrap();
        assert!(left.x < 40.0);
        assert_eq!(tap_point(rect, |_| false), None);
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

    #[test]
    fn a_long_press_without_touch_is_a_right_click() {
        let plan = |touch, long| Plan {
            control: control(1, "button", "A"),
            at: egui::pos2(5.0, 5.0),
            touch,
            long,
        };
        assert_eq!(plan(false, true).button(), egui::PointerButton::Secondary);
        assert_eq!(plan(true, true).button(), egui::PointerButton::Primary);
        let down = down_events(&plan(false, false));
        assert!(!down.iter().any(|e| matches!(e, egui::Event::Touch { .. })));
        let up = up_events(&plan(true, false));
        assert!(matches!(
            up.first(),
            Some(egui::Event::Touch {
                phase: egui::TouchPhase::End,
                ..
            })
        ));
        assert!(matches!(up.last(), Some(egui::Event::PointerGone)));
    }

    // --- Through a real egui context -------------------------------------

    struct Panel {
        on: bool,
        text: String,
        clicks: u32,
        long_touches: u32,
    }

    fn draw(ui: &mut egui::Ui, p: &mut Panel) {
        let r = ui.button("Open settings");
        if r.clicked() {
            p.clicks += 1;
        }
        if r.long_touched() {
            p.long_touches += 1;
        }
        ui.checkbox(&mut p.on, "Show buildings");
        ui.add(egui::TextEdit::singleline(&mut p.text).hint_text("Message"));
    }

    fn pass(ctx: &egui::Context, time: f64, p: &mut Panel) {
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            )),
            time: Some(time),
            ..Default::default()
        };
        let _ = ctx.run_ui_unpainted(raw, |ui| draw(ui, p));
    }

    fn hooked() -> (egui::Context, Panel) {
        let ctx = egui::Context::default();
        ctx.add_plugin(ScreenHook::default());
        ctx.enable_accesskit();
        let panel = Panel {
            on: false,
            text: "meet me at the station".into(),
            clicks: 0,
            long_touches: 0,
        };
        (ctx, panel)
    }

    fn look(ctx: &egui::Context, p: &mut Panel, time: f64) -> Snapshot {
        ask_snapshot(ctx, 1, false);
        pass(ctx, time, p);
        with_hook(ctx, |h| h.taken.take())
            .flatten()
            .expect("a snapshot")
    }

    #[test]
    fn the_tree_names_controls_and_not_what_was_typed() {
        let (ctx, mut p) = hooked();
        pass(&ctx, 0.0, &mut p);
        let snap = look(&ctx, &mut p, 0.1);
        let find = |label: &str| snap.controls.iter().find(|c| c.label == label);
        assert_eq!(find("Open settings").map(|c| c.kind), Some("button"));
        let tick = find("Show buildings").expect("checkbox");
        assert_eq!((tick.kind, tick.selected), ("checkbox", Some(false)));
        assert_eq!(find("Message").map(|c| c.kind), Some("text field"));
        let all = format!("{:?}", tree_reply(&snap, None).data);
        assert!(!all.contains("station"), "{all}");
        assert!(!snap.controls.iter().any(|c| c.label.contains("station")));
    }

    #[test]
    fn a_press_clicks_the_control_under_it() {
        for touch in [false, true] {
            let (ctx, mut p) = hooked();
            pass(&ctx, 0.0, &mut p);
            let snap = look(&ctx, &mut p, 0.1);
            let button = resolve(&snap.controls, &Target::Label("open settings".into()))
                .unwrap()
                .clone();
            let plan = Plan {
                at: button.tap.unwrap(),
                control: button,
                touch,
                long: false,
            };
            with_hook(&ctx, |h| h.inject.extend(down_events(&plan)));
            pass(&ctx, 0.2, &mut p);
            with_hook(&ctx, |h| h.inject.extend(up_events(&plan)));
            pass(&ctx, 0.3, &mut p);
            assert_eq!(p.clicks, 1, "touch: {touch}");
        }
    }

    #[test]
    fn a_held_finger_is_a_long_touch() {
        let (ctx, mut p) = hooked();
        pass(&ctx, 0.0, &mut p);
        let snap = look(&ctx, &mut p, 0.1);
        let button = resolve(&snap.controls, &Target::Label("Open settings".into()))
            .unwrap()
            .clone();
        let plan = Plan {
            at: button.tap.unwrap(),
            control: button,
            touch: true,
            long: true,
        };
        with_hook(&ctx, |h| h.inject.extend(down_events(&plan)));
        let hold = ctx.options(|o| o.input_options.max_click_duration) + LONG_MARGIN_S;
        let mut t = 0.2;
        while t < 0.2 + hold + 0.1 {
            pass(&ctx, t, &mut p);
            t += 0.1;
        }
        with_hook(&ctx, |h| h.inject.extend(up_events(&plan)));
        pass(&ctx, t, &mut p);
        assert_eq!(p.long_touches, 1);
    }
}
