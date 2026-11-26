//! The mounted tree as an assistive technology sees it: what each visible
//! node is (a button, a field, a heading…), what it is called, its state and
//! where it stands on the screen — and a way to press one as a finger would.
//! The client's MCP tools `ui_tree` and `press` read the screen through this.
//!
//! **Names follow the browser's rules, in short**: `aria-label`, then
//! `aria-labelledby`, then a `<label for>` or a wrapping `<label>`, then
//! `title`, then (a field) its `placeholder` or (an image) its `alt`.
//! What a node holds as text is returned apart ([`AccessNode::text`]), so
//! the caller decides whether inner text names it — a row in a chat list
//! is somebody's words.
//!
//! **Rects are logical pixels**, the units the tree's styles are written
//! in: physical pixels times `ComputedNode::inverse_scale_factor`, which
//! folds in `UiScale`. Clipped to what a scroll area or an `overflow:
//! hidden` ancestor shows.
//!
//! **A press is the picking backend's events, not a handler call.** The
//! point is hit-tested the way `bevy_ui`'s picking backend does it (the
//! `UiStack` from the top, a node's rounded box and its clip, a
//! non-blocking `Pickable` letting the hit through), and `PointerPress`,
//! `PointerRelease` and `PointerClick` are triggered on what it hits, so
//! they bubble through the same observers a real pointer's do: a covered
//! control cannot be pressed, a disabled one ignores it, a checkbox toggles,
//! a select opens its popup, a text field takes the focus.

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::time::Duration;

use bevy::camera::{NormalizedRenderTarget, RenderTarget};
use bevy::input_focus::InputFocus;
use bevy::picking::backend::HitData;
use bevy::picking::events::{Pointer, PointerClick, PointerPress, PointerRelease};
use bevy::picking::pointer::{Location, PointerButton, PointerId};
use bevy::prelude::*;
use bevy::text::EditableText;
use bevy::ui::{CalculatedClip, ComputedNode, UiGlobalTransform, UiStack};
use mithril::style::cascade::Display;

use crate::tree::{select_options, NodeKind, ShadowNode, UiTree};
use crate::typing::{type_into_focused, KeyEdit};
use crate::{with_host, NodeId, UiCamera, ROOT};

/// What a node is to someone who cannot see it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Role {
    Button,
    Link,
    CheckBox,
    Switch,
    Radio,
    Tab,
    /// A text input or a textarea.
    TextField,
    /// A range input.
    Slider,
    /// A `<select>`.
    Select,
    /// An option of a list (`role="option"`).
    Option,
    MenuItem,
    /// Anything else with a click handler: a row, a card, a tile.
    Clickable,
    Heading,
    Image,
    /// Text that is not inside a control.
    Text,
}

impl Role {
    /// Whether the role is something to press or type into.
    pub fn is_control(self) -> bool {
        !matches!(self, Role::Heading | Role::Image | Role::Text)
    }
}

/// One node of the screen, as [`access_tree`] reads it.
#[derive(Clone, Debug)]
pub struct AccessNode {
    pub node: NodeId,
    pub entity: Entity,
    /// Stable while the node's place in the tree is (the tags, classes and
    /// positions of it and its ancestors): a screen that mounts again gets
    /// new [`NodeId`]s but the same keys.
    pub key: u64,
    pub role: Role,
    /// Its own name (see the module note); `None` when it has none.
    pub label: Option<String>,
    /// The text it holds, a piece per text node, icons and `aria-hidden`
    /// parts left out; not the text inside a control it holds.
    pub text: Vec<String>,
    /// It holds another control: a container rather than one control.
    pub holds_controls: bool,
    /// A text field's text, a select's chosen option, a slider's value.
    pub value: Option<String>,
    /// A checkbox's, switch's or radio's tick; `aria-pressed`,
    /// `aria-selected` or `aria-checked` on anything else.
    pub checked: Option<bool>,
    pub enabled: bool,
    /// The part of it on screen, logical px.
    pub rect: Rect,
    /// Where a press reaches it — its centre, or another point of it when
    /// the centre is under something else; `None` when none of it can be
    /// reached. Logical px.
    pub tap: Option<Vec2>,
}

/// The screen the tree lays out in, logical px: the root's box.
pub fn screen_rect(world: &World) -> Option<Rect> {
    let root = world.resource::<UiTree>().root?;
    let c = world.get::<ComputedNode>(root)?;
    let size = c.size() * c.inverse_scale_factor();
    let centre = world
        .get::<UiGlobalTransform>(root)
        .map_or(size / 2.0, |t| t.translation * c.inverse_scale_factor());
    Some(Rect::from_center_size(centre, size))
}

fn attr<'a>(n: &'a ShadowNode, name: &str) -> Option<&'a str> {
    match &n.kind {
        NodeKind::Element { attrs, .. } => attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str()),
        _ => None,
    }
}

fn tag(n: &ShadowNode) -> &str {
    match &n.kind {
        NodeKind::Element { tag, .. } => tag,
        _ => "",
    }
}

fn non_empty(s: Option<&str>) -> Option<String> {
    s.map(clean).filter(|s| !s.is_empty())
}

/// Text with the icon font's glyphs (the private use areas) taken out and
/// its whitespace folded.
fn clean(s: &str) -> String {
    s.chars()
        .filter(|c| {
            let c = *c as u32;
            !((0xE000..=0xF8FF).contains(&c) || c >= 0xF0000)
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn aria_bool(n: &ShadowNode, name: &str) -> Option<bool> {
    match attr(n, name)? {
        "true" | "" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

/// Hidden from layout or from sight: `display: none`, `visibility:
/// hidden` (Bevy hides the whole subtree), or Bevy's own visibility.
fn hidden(world: &World, n: &ShadowNode) -> bool {
    if n.computed
        .as_ref()
        .is_some_and(|c| c.display == Display::None || !c.visible)
    {
        return true;
    }
    let Some(e) = n.entity else {
        return false;
    };
    world.get::<Visibility>(e) == Some(&Visibility::Hidden)
        || world
            .get::<InheritedVisibility>(e)
            .is_some_and(|v| !v.get())
        || world
            .get::<Node>(e)
            .is_some_and(|node| node.display == bevy::ui::Display::None)
}

/// The role a node has, or `None` for a node that is only structure.
fn role_of(n: &ShadowNode, listened: &HashSet<NodeId>, id: NodeId) -> Option<Role> {
    let NodeKind::Element { tag, .. } = &n.kind else {
        return None;
    };
    let ty = attr(n, "type").unwrap_or("");
    match attr(n, "role").unwrap_or("") {
        "button" => return Some(Role::Button),
        "link" => return Some(Role::Link),
        "tab" => return Some(Role::Tab),
        "switch" => return Some(Role::Switch),
        "checkbox" => return Some(Role::CheckBox),
        "radio" => return Some(Role::Radio),
        "option" => return Some(Role::Option),
        "menuitem" | "menuitemcheckbox" | "menuitemradio" => return Some(Role::MenuItem),
        "slider" => return Some(Role::Slider),
        "textbox" | "searchbox" => return Some(Role::TextField),
        "heading" => return Some(Role::Heading),
        _ => {}
    }
    Some(match tag.as_str() {
        "button" => Role::Button,
        "a" => Role::Link,
        "select" => Role::Select,
        "textarea" => Role::TextField,
        "input" => match ty {
            "hidden" => return None,
            "checkbox" => Role::CheckBox,
            "radio" => Role::Radio,
            "range" => Role::Slider,
            "button" | "submit" | "reset" => Role::Button,
            _ => Role::TextField,
        },
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => Role::Heading,
        "img" => Role::Image,
        _ if listened.contains(&id) => Role::Clickable,
        _ => return None,
    })
}

/// The pieces of text under `id`, and whether a control sits under it.
fn inner_text(
    tree: &UiTree,
    world: &World,
    id: NodeId,
    listened: &HashSet<NodeId>,
) -> (Vec<String>, bool) {
    let mut out = Vec::new();
    let mut nested = false;
    let Some(n) = tree.nodes.get(&id) else {
        return (out, nested);
    };
    let mut stack: Vec<NodeId> = n.children.iter().rev().copied().collect();
    while let Some(c) = stack.pop() {
        let Some(cn) = tree.nodes.get(&c) else {
            continue;
        };
        if hidden(world, cn) {
            continue;
        }
        match &cn.kind {
            NodeKind::Text(t) => {
                let t = clean(t);
                if !t.is_empty() {
                    out.push(t);
                }
            }
            NodeKind::Element { .. } => {
                if aria_bool(cn, "aria-hidden") == Some(true) {
                    continue;
                }
                if role_of(cn, listened, c).is_some_and(Role::is_control) {
                    nested = true;
                    continue;
                }
                stack.extend(cn.children.iter().rev().copied());
            }
            NodeKind::Anchor => {}
        }
    }
    (out, nested)
}

/// Element ids (`id="…"`) to nodes, and `<label for>` targets to labels.
struct Ids {
    by_id: HashMap<String, NodeId>,
    label_for: HashMap<String, NodeId>,
}

fn collect_ids(tree: &UiTree) -> Ids {
    let mut ids = Ids {
        by_id: HashMap::new(),
        label_for: HashMap::new(),
    };
    for (nid, n) in &tree.nodes {
        if let Some(v) = attr(n, "id") {
            ids.by_id.insert(v.to_owned(), *nid);
        }
        if tag(n) == "label" {
            if let Some(v) = attr(n, "for") {
                ids.label_for.insert(v.to_owned(), *nid);
            }
        }
    }
    ids
}

fn own_label(
    tree: &UiTree,
    world: &World,
    n: &ShadowNode,
    role: Role,
    ids: &Ids,
    listened: &HashSet<NodeId>,
) -> Option<String> {
    let text_of = |other: NodeId| {
        let (pieces, _) = inner_text(tree, world, other, listened);
        let joined = pieces.join(" ");
        (!joined.is_empty()).then_some(joined)
    };
    if let Some(l) = non_empty(attr(n, "aria-label")) {
        return Some(l);
    }
    if let Some(by) = attr(n, "aria-labelledby") {
        let joined: Vec<String> = by
            .split_whitespace()
            .filter_map(|i| ids.by_id.get(i).copied())
            .filter_map(text_of)
            .collect();
        if !joined.is_empty() {
            return Some(joined.join(" "));
        }
    }
    if let Some(l) = attr(n, "id")
        .and_then(|i| ids.label_for.get(i).copied())
        .and_then(text_of)
    {
        return Some(l);
    }
    if matches!(
        role,
        Role::TextField | Role::CheckBox | Role::Radio | Role::Switch | Role::Slider | Role::Select
    ) {
        // A control inside a `<label>`: the label's other text.
        let mut cur = n.parent;
        while let Some(p) = cur {
            let Some(pn) = tree.nodes.get(&p) else { break };
            if tag(pn) == "label" {
                if let Some(l) = text_of(p) {
                    return Some(l);
                }
                break;
            }
            cur = pn.parent;
        }
    }
    if let Some(l) = non_empty(attr(n, "title")) {
        return Some(l);
    }
    match role {
        Role::TextField => non_empty(attr(n, "placeholder")).or_else(|| match &n.kind {
            NodeKind::Element { props, .. } => {
                non_empty(props.get("placeholder").map(String::as_str))
            }
            _ => None,
        }),
        Role::Image => non_empty(attr(n, "alt")),
        _ => None,
    }
}

fn value_of(
    world: &World,
    tree: &UiTree,
    id: NodeId,
    n: &ShadowNode,
    role: Role,
) -> Option<String> {
    let NodeKind::Element { props, .. } = &n.kind else {
        return None;
    };
    match role {
        Role::TextField => n
            .entity
            .and_then(|e| world.get::<EditableText>(e))
            .map(|ed| ed.editor.text().to_string())
            .or_else(|| props.get("value").cloned()),
        Role::Slider => n
            .entity
            .and_then(|e| world.get::<bevy::ui_widgets::SliderValue>(e))
            .map(|v| format!("{}", v.0))
            .or_else(|| props.get("value").cloned()),
        Role::Select => {
            let i = props
                .get("selectedIndex")
                .and_then(|v| v.parse::<usize>().ok())?;
            select_options(tree, id)
                .get(i)
                .map(|(label, _)| clean(label))
                .filter(|l| !l.is_empty())
        }
        _ => None,
    }
}

fn checked_of(n: &ShadowNode, role: Role) -> Option<bool> {
    let NodeKind::Element { states, .. } = &n.kind else {
        return None;
    };
    if matches!(role, Role::CheckBox | Role::Radio | Role::Switch) && tag(n) == "input" {
        return Some(states.checked);
    }
    aria_bool(n, "aria-checked")
        .or_else(|| aria_bool(n, "aria-pressed"))
        .or_else(|| aria_bool(n, "aria-selected"))
}

/// The node's box on screen, physical px, clipped; `None` when nothing of
/// it shows.
fn physical_rect(world: &World, e: Entity) -> Option<Rect> {
    let c = world.get::<ComputedNode>(e)?;
    let t = world.get::<UiGlobalTransform>(e)?;
    let size = c.size();
    if size.x <= 0.0 || size.y <= 0.0 {
        return None;
    }
    let mut r = Rect::from_center_size(t.translation, size);
    match world.get::<CalculatedClip>(e) {
        Some(CalculatedClip::FullyClipped) => return None,
        Some(CalculatedClip::Rects(rects)) => {
            for clip in rects {
                let to_world = clip.world_to_clip_local.inverse();
                let a = to_world.transform_point2(clip.rect.min);
                let b = to_world.transform_point2(clip.rect.max);
                r = r.intersect(Rect::from_corners(a, b));
            }
        }
        None => {}
    }
    (r.width() > 0.5 && r.height() > 0.5).then_some(r)
}

/// What a press at `p` (physical px) reaches, top first — `bevy_ui`'s
/// picking backend, over this tree's nodes: the stack from the top, the
/// first node whose box and clip hold the point, and the ones under it
/// while a `Pickable` lets the press through. Only hoverable ones.
fn hits_at(world: &World, p: Vec2) -> Vec<Entity> {
    let tree = world.resource::<UiTree>();
    let Some(stack) = world.get_resource::<UiStack>() else {
        return Vec::new();
    };
    let ours = |e: Entity| tree.by_entity.contains_key(&e) || Some(e) == tree.root;
    let mut out = Vec::new();
    for range in stack.partition.iter().rev() {
        for &e in stack.uinodes[range.clone()].iter().rev() {
            if !ours(e) {
                continue;
            }
            let (Some(c), Some(t)) = (
                world.get::<ComputedNode>(e),
                world.get::<UiGlobalTransform>(e),
            ) else {
                continue;
            };
            if c.size() == Vec2::ZERO {
                continue;
            }
            if world
                .get::<InheritedVisibility>(e)
                .is_some_and(|v| !v.get())
                || world.get::<Visibility>(e) == Some(&Visibility::Hidden)
            {
                continue;
            }
            if !c.contains_point(*t, p)
                || !world
                    .get::<CalculatedClip>(e)
                    .is_none_or(|clip| clip.contains_point(p))
            {
                continue;
            }
            let pickable = world.get::<Pickable>(e);
            if pickable.is_none_or(|pk| pk.is_hoverable) {
                out.push(e);
            }
            if pickable.is_none_or(|pk| pk.should_block_lower) {
                return out;
            }
        }
    }
    out
}

/// Whether `e` is `target` or under it: a press on it bubbles to `target`.
fn within(world: &World, mut e: Entity, target: Entity) -> bool {
    loop {
        if e == target {
            return true;
        }
        match world.get::<ChildOf>(e) {
            Some(p) => e = p.parent(),
            None => return false,
        }
    }
}

/// The first of nine spots in `r` (physical) a press reaches `target` at.
/// With no `UiStack` (nothing laid out by `bevy_ui`), the centre.
fn tap_point(world: &World, r: Rect, target: Entity) -> Option<Vec2> {
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
    if world.get_resource::<UiStack>().is_none() {
        return Some(r.center());
    }
    SPOTS
        .iter()
        .map(|&(x, y)| r.min + r.size() * Vec2::new(x, y))
        .find(|&p| hits_at(world, p).iter().any(|&h| within(world, h, target)))
}

/// Nodes the tree listens to a press or an edit on.
fn listened() -> HashSet<NodeId> {
    with_host(|host| {
        host.handlers
            .keys()
            .filter(|(_, ev)| matches!(ev.as_str(), "click" | "change" | "input"))
            .map(|(id, _)| *id)
            .collect()
    })
}

/// Every visible node that is a control, a heading, an image or text
/// outside a control, in tree order. Hidden subtrees (`display: none`,
/// `visibility: hidden`) are left out, and so are nodes with nothing on
/// screen (zero size, scrolled or clipped away).
///
/// Main thread only: it reads which nodes have handlers from the host.
pub fn access_tree(world: &World) -> Vec<AccessNode> {
    let Some(tree) = world.get_resource::<UiTree>() else {
        return Vec::new();
    };
    if tree.root.is_none() {
        return Vec::new();
    }
    let listened = listened();
    let ids = collect_ids(tree);
    let mut out = Vec::new();
    // (node, the key of its parent's path, inside a control).
    let mut stack: Vec<(NodeId, u64, bool)> = vec![(ROOT, 0, false)];
    while let Some((id, parent_key, in_control)) = stack.pop() {
        let Some(n) = tree.nodes.get(&id) else {
            continue;
        };
        if id != ROOT && hidden(world, n) {
            continue;
        }
        if aria_bool(n, "aria-hidden") == Some(true) {
            continue;
        }
        // The path key: the parent's, this node's tag and classes, its id
        // attribute, and its place among its element siblings.
        let key = {
            let mut h = DefaultHasher::new();
            parent_key.hash(&mut h);
            if let NodeKind::Element { tag, classes, .. } = &n.kind {
                tag.hash(&mut h);
                classes.hash(&mut h);
            }
            attr(n, "id").hash(&mut h);
            let index = n.parent.and_then(|p| {
                tree.nodes
                    .get(&p)?
                    .children
                    .iter()
                    .filter(|c| {
                        tree.nodes
                            .get(c)
                            .is_some_and(|cn| matches!(cn.kind, NodeKind::Element { .. }))
                    })
                    .position(|c| *c == id)
            });
            index.hash(&mut h);
            h.finish()
        };
        let role = match &n.kind {
            NodeKind::Text(t) if !in_control && !clean(t).is_empty() => Some(Role::Text),
            _ => role_of(n, &listened, id),
        };
        let mut child_in_control = in_control;
        if let (Some(role), Some(e)) = (role, n.entity) {
            // A control's or a heading's text is its name, not text of
            // its own.
            if role.is_control() || role == Role::Heading {
                child_in_control = true;
            }
            if let Some(phys) = physical_rect(world, e) {
                let inv = world
                    .get::<ComputedNode>(e)
                    .map_or(1.0, |c| c.inverse_scale_factor());
                let (text, holds_controls) = match &n.kind {
                    NodeKind::Text(t) => (vec![clean(t)], false),
                    _ => inner_text(tree, world, id, &listened),
                };
                let enabled = match &n.kind {
                    NodeKind::Element { .. } => {
                        attr(n, "disabled").is_none()
                            && aria_bool(n, "aria-disabled") != Some(true)
                            && world.get::<bevy::ui::InteractionDisabled>(e).is_none()
                    }
                    _ => true,
                };
                out.push(AccessNode {
                    node: id,
                    entity: e,
                    key,
                    role,
                    label: own_label(tree, world, n, role, &ids, &listened),
                    text,
                    holds_controls,
                    value: value_of(world, tree, id, n, role),
                    checked: checked_of(n, role),
                    enabled,
                    rect: Rect::from_corners(phys.min * inv, phys.max * inv),
                    tap: role
                        .is_control()
                        .then(|| tap_point(world, phys, e))
                        .flatten()
                        .map(|p| p * inv),
                });
            }
        }
        for c in n.children.iter().rev() {
            stack.push((*c, key, child_in_control));
        }
    }
    out
}

/// Why a press did not happen.
#[derive(Debug, PartialEq, Eq)]
pub enum PressError {
    /// No such node in the tree now.
    Gone,
    /// The point does not reach it: something lies over it there.
    Covered,
}

/// The pointer a press comes from: not a mouse or a platform's finger, so
/// nothing that tracks those sees it.
const PRESS_POINTER: PointerId = PointerId::Touch(u64::MAX - 7);

/// Presses `node` at `at` (logical px) as a finger would: down, up, and the
/// click, triggered on whatever the point hits, so the events bubble to the
/// node's observers as a real pointer's do. The handlers run in the next
/// [`crate::drive_ui`].
pub fn press(world: &mut World, node: NodeId, at: Vec2) -> Result<(), PressError> {
    let entity = world
        .resource::<UiTree>()
        .nodes
        .get(&node)
        .and_then(|n| n.entity)
        .ok_or(PressError::Gone)?;
    let inv = world
        .get::<ComputedNode>(entity)
        .map_or(1.0, |c| c.inverse_scale_factor())
        .max(f32::EPSILON);
    let p = at / inv;
    let hits: Vec<Entity> = if world.get_resource::<UiStack>().is_some() {
        hits_at(world, p)
    } else {
        vec![entity]
    };
    if !hits.iter().any(|&h| within(world, h, entity)) {
        return Err(PressError::Covered);
    }
    let camera = world.get_resource::<UiCamera>().map(|c| c.0);
    let window = world
        .query_filtered::<Entity, With<bevy::window::PrimaryWindow>>()
        .iter(world)
        .next();
    let target = camera
        .and_then(|c| world.get::<RenderTarget>(c))
        .and_then(|t| t.normalize(window))
        .unwrap_or(NormalizedRenderTarget::None {
            width: 0,
            height: 0,
        });
    let pointer = Pointer::new(
        PRESS_POINTER,
        Location {
            target,
            position: at,
        },
    );
    for e in hits {
        let hit = HitData::new(camera.unwrap_or(Entity::PLACEHOLDER), 0.0, None, None);
        world.trigger(PointerPress {
            entity: e,
            pointer: pointer.clone(),
            button: PointerButton::Primary,
            hit: hit.clone(),
            count: 1,
        });
        world.trigger(PointerRelease {
            entity: e,
            pointer: pointer.clone(),
            button: PointerButton::Primary,
            hit: hit.clone(),
        });
        world.trigger(PointerClick {
            entity: e,
            pointer: pointer.clone(),
            button: PointerButton::Primary,
            hit,
            duration: Duration::from_millis(80),
            count: 1,
        });
    }
    Ok(())
}

/// Replaces a text field's text, as selecting all and typing would: the
/// field takes the focus, and its `input` (and a `keydown` per edit) reach
/// the handlers once `bevy_text` applies the edits. False when `node` is
/// not a text field.
pub fn set_text(world: &mut World, node: NodeId, text: &str) -> bool {
    let Some(entity) = world
        .resource::<UiTree>()
        .nodes
        .get(&node)
        .and_then(|n| n.entity)
    else {
        return false;
    };
    if world.get::<EditableText>(entity).is_none() {
        return false;
    }
    world
        .get_resource_or_init::<InputFocus>()
        .set(entity, bevy::input_focus::FocusCause::Navigated);
    type_into_focused(world, KeyEdit::SelectAll);
    if text.is_empty() {
        type_into_focused(world, KeyEdit::Backspace)
    } else {
        type_into_focused(world, KeyEdit::Text(text.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    use mithril::view::{el, IntoView};

    use super::*;
    use crate::drive_ui;
    use crate::testing::{lay_out_rows, ROW};
    use mithril::view::View;

    struct Seen {
        saves: Rc<Cell<u32>>,
        closes: Rc<Cell<u32>>,
        ticked: Rc<RefCell<Vec<Option<bool>>>>,
    }

    /// A small panel: a heading, a text button, an icon button named by
    /// `aria-label`, a labelled field, a switch, a disabled button, and two
    /// hidden ones.
    fn mounted() -> (World, Seen) {
        let mut world = crate::testing::world();
        let seen = Seen {
            saves: Rc::new(Cell::new(0)),
            closes: Rc::new(Cell::new(0)),
            ticked: Rc::new(RefCell::new(Vec::new())),
        };
        with_host(|h| h.mount(panel(&seen)));
        drive_ui(&mut world);
        (world, seen)
    }

    fn panel(seen: &Seen) -> View {
        let (s, c, t) = (seen.saves.clone(), seen.closes.clone(), seen.ticked.clone());
        el("div")
            .class("panel")
            .child(el("h2").child("Settings"))
            .child(
                el("button")
                    .attr("type", "button")
                    .on("click", move |_| s.set(s.get() + 1))
                    .child("Save"),
            )
            .child(
                el("button")
                    .attr("type", "button")
                    .attr("aria-label", "Close")
                    .on("click", move |_| c.set(c.get() + 1))
                    .child(el("span").attr("aria-hidden", "true").child("\u{e5cd}")),
            )
            .child(el("label").attr("for", "name").child("Your name"))
            .child(
                el("input")
                    .attr("id", "name")
                    .attr("type", "text")
                    .attr("placeholder", "Ada")
                    .prop("value", "Grace")
                    .on("input", |_| {}),
            )
            .child(
                el("input")
                    .attr("type", "checkbox")
                    .attr("role", "switch")
                    .attr("aria-label", "Show trees")
                    .prop_bool("checked", true)
                    .on("change", move |ev| t.borrow_mut().push(ev.checked)),
            )
            .child(
                el("button")
                    .attr("type", "button")
                    .attr("disabled", true)
                    .on("click", |_| {})
                    .child("Off"),
            )
            .child(
                el("button")
                    .attr("style", "display: none")
                    .on("click", |_| {})
                    .child("Gone"),
            )
            .child(
                el("div")
                    .attr("style", "visibility: hidden")
                    .child(el("button").on("click", |_| {}).child("Ghost")),
            )
            .into_view()
    }

    fn find<'a>(nodes: &'a [AccessNode], label: &str) -> Option<&'a AccessNode> {
        let named = |n: &&AccessNode| {
            n.label.as_deref() == Some(label) || (n.label.is_none() && n.text.join(" ") == label)
        };
        // A control before the text that names it (a `<label for>`).
        nodes
            .iter()
            .filter(|n| n.role.is_control())
            .find(named)
            .or_else(|| nodes.iter().find(named))
    }

    #[test]
    fn the_walk_names_roles_states_and_rects() {
        let (mut world, _) = mounted();
        lay_out_rows(&mut world, 2.0);
        let nodes = access_tree(&world);
        let heading = find(&nodes, "Settings").expect("heading");
        assert_eq!(heading.role, Role::Heading);
        let save = find(&nodes, "Save").expect("save");
        assert_eq!(save.role, Role::Button);
        assert!(save.label.is_none() && save.enabled && !save.holds_controls);
        let close = find(&nodes, "Close").expect("close");
        assert_eq!(close.role, Role::Button);
        // The icon glyph is neither its name nor its text.
        assert!(close.text.is_empty(), "{:?}", close.text);
        let field = find(&nodes, "Your name").expect("field by its <label for>");
        assert_eq!(field.role, Role::TextField);
        assert_eq!(field.value.as_deref(), Some("Grace"));
        let switch = find(&nodes, "Show trees").expect("switch");
        assert_eq!((switch.role, switch.checked), (Role::Switch, Some(true)));
        let off = find(&nodes, "Off").expect("disabled button");
        assert!(!off.enabled);
        assert!(find(&nodes, "Gone").is_none(), "display: none is left out");
        assert!(
            find(&nodes, "Ghost").is_none(),
            "visibility: hidden is left out"
        );
        // The heading's and the buttons' text is theirs, not listed again.
        assert!(!nodes
            .iter()
            .any(|n| n.role == Role::Text && n.text.join(" ") == "Save"));
        // Logical px: the rows are physical, at two physical px a point.
        assert_eq!(save.rect.width(), 100.0);
        assert_eq!(save.rect.height(), (ROW - 8.0) / 2.0);
        assert_eq!(save.tap, Some(save.rect.center()));
        let screen = screen_rect(&world).expect("screen");
        assert_eq!(screen.width(), 200.0);
    }

    #[test]
    fn keys_survive_a_remount_and_ids_do_not() {
        let (mut world, seen) = mounted();
        lay_out_rows(&mut world, 1.0);
        let before = find(&access_tree(&world), "Save").unwrap().clone();
        with_host(|h| h.clear());
        drive_ui(&mut world);
        assert!(access_tree(&world).is_empty());
        with_host(|h| h.mount(panel(&seen)));
        drive_ui(&mut world);
        lay_out_rows(&mut world, 1.0);
        let after = find(&access_tree(&world), "Save").unwrap().clone();
        assert_eq!(before.key, after.key);
        assert_ne!(before.node, after.node);
    }

    #[test]
    fn a_press_reaches_the_handlers_through_the_pointer_events() {
        let (mut world, seen) = mounted();
        lay_out_rows(&mut world, 2.0);
        let nodes = access_tree(&world);
        let save = find(&nodes, "Save").unwrap().clone();
        press(&mut world, save.node, save.tap.unwrap()).unwrap();
        let close = find(&nodes, "Close").unwrap().clone();
        press(&mut world, close.node, close.tap.unwrap()).unwrap();
        let switch = find(&nodes, "Show trees").unwrap().clone();
        press(&mut world, switch.node, switch.tap.unwrap()).unwrap();
        let off = find(&nodes, "Off").unwrap().clone();
        press(&mut world, off.node, off.tap.unwrap()).unwrap();
        drive_ui(&mut world);
        assert_eq!(seen.saves.get(), 1);
        assert_eq!(seen.closes.get(), 1);
        // A click on a ticked switch asks for it unticked.
        assert_eq!(*seen.ticked.borrow(), vec![Some(false)]);
    }

    #[test]
    fn a_covered_control_is_not_pressed() {
        let (mut world, seen) = mounted();
        lay_out_rows(&mut world, 1.0);
        let save = find(&access_tree(&world), "Save").unwrap().clone();
        // A sheet over the Save row, on top of the stack.
        let sheet = world
            .spawn((
                ComputedNode {
                    size: Vec2::new(400.0, ROW),
                    inverse_scale_factor: 1.0,
                    ..default()
                },
                UiGlobalTransform::from_translation(save.rect.center()),
            ))
            .id();
        world
            .resource_mut::<UiTree>()
            .by_entity
            .insert(sheet, NodeId(9_999));
        {
            let mut stack = world.resource_mut::<UiStack>();
            stack.uinodes.push(sheet);
            let len = stack.uinodes.len();
            stack.partition = vec![0..len];
        }
        let covered = find(&access_tree(&world), "Save").unwrap().clone();
        assert_eq!(covered.tap, None);
        assert_eq!(
            press(&mut world, save.node, save.rect.center()),
            Err(PressError::Covered)
        );
        drive_ui(&mut world);
        assert_eq!(seen.saves.get(), 0);
    }

    #[test]
    fn set_text_focuses_the_field_and_types() {
        let (mut world, _) = mounted();
        lay_out_rows(&mut world, 1.0);
        let nodes = access_tree(&world);
        let field = find(&nodes, "Your name").unwrap().clone();
        assert!(set_text(&mut world, field.node, "Hopper"));
        assert_eq!(world.resource::<InputFocus>().get(), Some(field.entity));
        let save = find(&nodes, "Save").unwrap();
        assert!(!set_text(&mut world, save.node, "x"));
    }
}
