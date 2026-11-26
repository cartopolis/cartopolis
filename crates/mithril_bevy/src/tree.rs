//! The shadow tree: what the view asked for per node (tag, classes,
//! attributes, properties, states, children), the entity behind each, and
//! the pass that turns the cascade's computed style into `bevy_ui`
//! components.

use std::collections::{HashMap, HashSet};

use bevy::input_focus::InputFocus;
use bevy::picking::events::PointerDrag;
use bevy::prelude::*;
use bevy::text::{EditableText, LineHeight};
use bevy::ui::ScrollPosition;
use bevy::ui::{
    BoxShadow, GlobalZIndex, GridTrack, Overflow, OverflowAxis, RepeatedGridTrack, ShadowStyle,
    UiTargetCamera, UiTransform, Val2,
};
use bevy::ui_widgets::{
    ScrollArea, Slider, SliderRange, SliderStep, SliderThumb, SliderValue, TextInput, TrackClick,
};
use mithril::style::cascade::{
    Chain, Computed, Display, Element, Length, Overflow as Ov, Position, Rgba, States, TextAlign,
};

use crate::{with_host, NodeId, Op, UiCamera, ROOT};

/// The fonts the renderer sets: the app maps the stylesheet's families and
/// weights onto its font assets.
#[derive(Resource, Clone)]
pub struct FontMap {
    pub regular: Handle<Font>,
    pub semibold: Handle<Font>,
    pub mono: Handle<Font>,
    /// An icon font, for a family naming "Symbols" or "icons"
    /// (`font-family: 'Material Symbols Rounded'`); `None` falls back to
    /// the regular face.
    pub icons: Option<Handle<Font>>,
    /// The icon face drawn this much larger than its `font-size`; 1 for none.
    pub icon_scale: f32,
    /// A face every text face falls back to for a character it lacks — an
    /// emoji font, so a 😮 in a chat is a face and not an empty box.
    pub emoji: Option<Handle<Font>>,
}

impl FontMap {
    fn is_icons(&self, family: &str) -> bool {
        self.icons.is_some() && (family.contains("Symbols") || family.contains("icons"))
    }

    /// The size `px` of `family` is drawn at.
    fn size(&self, family: &str, px: f32) -> f32 {
        if self.is_icons(family) {
            px * self.icon_scale
        } else {
            px
        }
    }

    fn pick(&self, family: &str, weight: u16) -> bevy::text::FontSource {
        if family.contains("Symbols") || family.contains("icons") {
            if let Some(icons) = &self.icons {
                return icons.clone().into();
            }
        }
        let face = if family.contains("monospace") || family.contains("mono") {
            self.mono.clone()
        } else if weight >= 550 {
            self.semibold.clone()
        } else {
            self.regular.clone()
        };
        match &self.emoji {
            Some(emoji) => bevy::text::FontSource::list([face, emoji.clone()]),
            None => face.into(),
        }
    }
}

pub(crate) enum NodeKind {
    Element {
        tag: String,
        classes: Vec<String>,
        attrs: Vec<(String, String)>,
        props: HashMap<String, String>,
        states: States,
    },
    Text(String),
    Anchor,
}

pub(crate) struct ShadowNode {
    pub entity: Option<Entity>,
    pub kind: NodeKind,
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
    pub computed: Option<Computed>,
    pub opacity: f32,
    /// What the view said about this node changed: style it and write its
    /// components.
    pub dirty: bool,
    /// Something its selectors may read changed (an ancestor's class, a
    /// hover, the focus, the window): style it, and write only if the style,
    /// opacity or states come out different. A focus change marks the whole
    /// tree this way, and rewriting every node respawned every decoration
    /// and made Bevy lay out and shape every text again.
    pub restyle: bool,
    /// The states the components were last written with.
    pub written_states: Option<States>,
    /// The drawn pseudo-elements, replaced on restyle.
    pub decor: Vec<Entity>,
    /// For `<select>`: the open popup, if any.
    pub popup: Option<Entity>,
}

/// The tree, a resource; entities are made as nodes are inserted.
#[derive(Resource, Default)]
pub struct UiTree {
    pub(crate) nodes: HashMap<NodeId, ShadowNode>,
    pub(crate) root: Option<Entity>,
    pub(crate) by_entity: HashMap<Entity, NodeId>,
    /// How the host's `Env` was last seen, to restyle everything on a theme
    /// or size change.
    env_key: Option<(bool, bool, i32, i32, i32)>,
    /// The focused node and the tree's generation when the states were
    /// last synced with the focus.
    focus_key: Option<(Option<NodeId>, u64)>,
    /// Counts the inserts and removals, which can move a node under a new
    /// ancestor.
    generation: u64,
}

impl UiTree {
    /// The node an entity belongs to (the entity itself or a decoration's
    /// owner).
    pub fn node_of(&self, entity: Entity) -> Option<NodeId> {
        self.by_entity.get(&entity).copied()
    }

    fn element_mut(&mut self, id: NodeId) -> Option<&mut ShadowNode> {
        self.nodes.get_mut(&id)
    }

    pub(crate) fn mark_dirty(&mut self, id: NodeId) {
        if let Some(n) = self.nodes.get_mut(&id) {
            n.dirty = true;
        }
    }

    /// Marks a node dirty and everything under it for a restyle: its own
    /// attributes changed, and descendants' selectors may read them.
    fn mark_subtree(&mut self, id: NodeId) {
        self.mark_dirty(id);
        self.restyle_subtree(id);
    }

    /// Marks a node for a restyle.
    pub(crate) fn mark_restyle(&mut self, id: NodeId) {
        if let Some(n) = self.nodes.get_mut(&id) {
            n.restyle = true;
        }
    }

    /// Marks a node and everything under it for a restyle.
    pub(crate) fn restyle_subtree(&mut self, id: NodeId) {
        let mut stack = vec![id];
        while let Some(i) = stack.pop() {
            if let Some(n) = self.nodes.get_mut(&i) {
                n.restyle = true;
                stack.extend(n.children.iter().copied());
            }
        }
    }
}

pub(crate) fn ensure_root(world: &mut World) -> Option<Entity> {
    if let Some(root) = world.resource::<UiTree>().root {
        return Some(root);
    }
    let camera = world.get_resource::<UiCamera>().map(|c| c.0)?;
    let root = world
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(0.0),
                right: Val::Px(0.0),
                top: Val::Px(0.0),
                bottom: Val::Px(0.0),
                display: bevy::ui::Display::Block,
                ..default()
            },
            UiTargetCamera(camera),
            Pickable::IGNORE,
            Name::new("mithril root"),
        ))
        .id();
    let mut tree = world.resource_mut::<UiTree>();
    tree.root = Some(root);
    tree.nodes.insert(
        ROOT,
        ShadowNode {
            entity: Some(root),
            kind: NodeKind::Element {
                tag: "root".into(),
                classes: Vec::new(),
                attrs: Vec::new(),
                props: HashMap::new(),
                states: States::default(),
            },
            parent: None,
            children: Vec::new(),
            computed: None,
            opacity: 1.0,
            dirty: false,
            decor: Vec::new(),
            popup: None,
            restyle: false,
            written_states: None,
        },
    );
    Some(root)
}

pub(crate) fn apply_op(world: &mut World, op: Op) {
    // `drive_ui` made the root before the first op.
    if ensure_root(world).is_none() {
        return;
    }
    if matches!(op, Op::InsertBefore { .. } | Op::Remove { .. }) {
        world.resource_mut::<UiTree>().generation += 1;
    }
    match op {
        Op::CreateElement { id, tag, .. } => {
            let entity = world
                .spawn((Node::default(), Name::new(format!("<{tag}> #{}", id.0))))
                .id();
            let mut tree = world.resource_mut::<UiTree>();
            tree.by_entity.insert(entity, id);
            tree.nodes.insert(
                id,
                ShadowNode {
                    entity: Some(entity),
                    kind: NodeKind::Element {
                        tag,
                        classes: Vec::new(),
                        attrs: Vec::new(),
                        props: HashMap::new(),
                        states: States::default(),
                    },
                    parent: None,
                    children: Vec::new(),
                    computed: None,
                    opacity: 1.0,
                    dirty: true,
                    decor: Vec::new(),
                    popup: None,
                    restyle: false,
                    written_states: None,
                },
            );
        }
        Op::CreateText { id, text } => {
            let entity = world
                .spawn((
                    Text::new(text.clone()),
                    Name::new(format!("text #{}", id.0)),
                ))
                .id();
            let mut tree = world.resource_mut::<UiTree>();
            tree.by_entity.insert(entity, id);
            tree.nodes.insert(
                id,
                ShadowNode {
                    entity: Some(entity),
                    kind: NodeKind::Text(text),
                    parent: None,
                    children: Vec::new(),
                    computed: None,
                    opacity: 1.0,
                    dirty: true,
                    decor: Vec::new(),
                    popup: None,
                    restyle: false,
                    written_states: None,
                },
            );
        }
        Op::CreateAnchor { id } => {
            world.resource_mut::<UiTree>().nodes.insert(
                id,
                ShadowNode {
                    entity: None,
                    kind: NodeKind::Anchor,
                    parent: None,
                    children: Vec::new(),
                    computed: None,
                    opacity: 1.0,
                    dirty: false,
                    decor: Vec::new(),
                    popup: None,
                    restyle: false,
                    written_states: None,
                },
            );
        }
        Op::SetText { id, text } => {
            let entity = {
                let mut tree = world.resource_mut::<UiTree>();
                match tree.element_mut(id) {
                    Some(n) => {
                        if let NodeKind::Text(t) = &mut n.kind {
                            *t = text.clone();
                        }
                        n.dirty = true;
                        n.entity
                    }
                    None => None,
                }
            };
            if let Some(e) = entity {
                // A line cut to fit starts again from its new whole.
                if let Some(mut el) = world.get_mut::<crate::ellipsis::Ellipsis>(e) {
                    el.reset(text.clone());
                }
                if let Some(mut t) = world.get_mut::<Text>(e) {
                    t.0 = text;
                }
            }
        }
        Op::SetAttribute { id, name, value } => {
            let mut tree = world.resource_mut::<UiTree>();
            if let Some(n) = tree.element_mut(id) {
                if let NodeKind::Element { classes, attrs, .. } = &mut n.kind {
                    if name == "class" {
                        *classes = value
                            .as_deref()
                            .unwrap_or("")
                            .split_whitespace()
                            .map(str::to_owned)
                            .collect();
                    } else {
                        attrs.retain(|(n, _)| n != &name);
                        if let Some(v) = value {
                            attrs.push((name, v));
                        }
                    }
                }
            }
            tree.mark_subtree(id);
        }
        Op::SetPropertyStr { id, name, value } => {
            let mut tree = world.resource_mut::<UiTree>();
            if let Some(n) = tree.element_mut(id) {
                if let NodeKind::Element { props, .. } = &mut n.kind {
                    match value {
                        Some(v) => {
                            props.insert(name, v);
                        }
                        None => {
                            props.remove(&name);
                        }
                    }
                }
                n.dirty = true;
            }
        }
        // The renderer action `@focus`: the field takes the keyboard as it
        // mounts (a search panel's one field), as the DOM renderer's does.
        // The renderer action `@scroll-end`: the scroll area to its end (a
        // chat's latest line). Past the end is clamped by the layout, so
        // the end it reaches is the content's as laid out this frame.
        Op::SetPropertyBool { id, name, value } if name == "@scroll-end" => {
            let entity = world
                .resource::<UiTree>()
                .nodes
                .get(&id)
                .and_then(|n| n.entity);
            if let (true, Some(e)) = (value, entity) {
                if let Ok(mut em) = world.get_entity_mut(e) {
                    em.insert(ScrollPosition(Vec2::new(0.0, f32::MAX)));
                }
            }
        }
        Op::SetPropertyBool { id, name, value } if name == "@focus" => {
            let entity = world
                .resource::<UiTree>()
                .nodes
                .get(&id)
                .and_then(|n| n.entity);
            debug!(?id, value, ?entity, "mithril_bevy: @focus asked");
            if let (true, Some(e)) = (value, entity) {
                let present = world.get_resource::<InputFocus>().is_some();
                debug!(?e, present, "mithril_bevy: @focus");
                if let Some(mut focus) = world.get_resource_mut::<InputFocus>() {
                    focus.set(e, bevy::input_focus::FocusCause::Navigated);
                }
            }
        }
        Op::SetPropertyBool { id, name, value } => {
            let mut tree = world.resource_mut::<UiTree>();
            if let Some(n) = tree.element_mut(id) {
                if let NodeKind::Element { props, states, .. } = &mut n.kind {
                    match name.as_str() {
                        "checked" => states.checked = value,
                        "indeterminate" => states.indeterminate = value,
                        _ => {}
                    }
                    if value {
                        props.insert(name, String::new());
                    } else {
                        props.remove(&name);
                    }
                }
            }
            tree.mark_subtree(id);
        }
        Op::InsertBefore {
            parent,
            node,
            before,
        } => {
            // Entities under the parent, in order; anchors have none.
            let (parent_entity, ordered): (Option<Entity>, Vec<Entity>) = {
                let mut tree = world.resource_mut::<UiTree>();
                if let Some(old_parent) = tree.nodes.get(&node).and_then(|n| n.parent) {
                    if let Some(p) = tree.nodes.get_mut(&old_parent) {
                        p.children.retain(|c| *c != node);
                    }
                }
                let Some(p) = tree.nodes.get_mut(&parent) else {
                    return;
                };
                p.children.retain(|c| *c != node);
                let index = before
                    .and_then(|b| p.children.iter().position(|c| *c == b))
                    .unwrap_or(p.children.len());
                p.children.insert(index, node);
                let parent_entity = p.entity;
                let children = p.children.clone();
                if let Some(n) = tree.nodes.get_mut(&node) {
                    n.parent = Some(parent);
                }
                let ordered = children
                    .iter()
                    .filter_map(|c| tree.nodes.get(c).and_then(|n| n.entity))
                    .collect();
                (parent_entity, ordered)
            };
            if let Some(pe) = parent_entity {
                // A parent's order is its whole child list; the decorations
                // come first so they draw under the content.
                let decor = world
                    .resource::<UiTree>()
                    .nodes
                    .get(&parent)
                    .map(|n| n.decor.clone())
                    .unwrap_or_default();
                let mut all = decor;
                all.extend(ordered);
                // In place, never `remove::<Children>` + `add_children`:
                // bevy_ui's layout syncs a changed child list and then
                // clears every parent whose `Children` was removed that
                // frame, so a remove-and-add leaves the subtree out of the
                // layout tree for good (every node 0×0 at (0,0)).
                world.entity_mut(pe).replace_children(&all);
            }
            world.resource_mut::<UiTree>().mark_subtree(node);
            world.resource_mut::<UiTree>().mark_dirty(parent);
        }
        Op::Remove { parent, node } => {
            let removed: Vec<Entity> = {
                let mut tree = world.resource_mut::<UiTree>();
                if let Some(p) = tree.nodes.get_mut(&parent) {
                    p.children.retain(|c| *c != node);
                }
                // Collect the subtree's entities and forget its nodes.
                let mut out = Vec::new();
                let mut stack = vec![node];
                while let Some(i) = stack.pop() {
                    if let Some(n) = tree.nodes.remove(&i) {
                        if let Some(e) = n.entity {
                            out.push(e);
                            tree.by_entity.remove(&e);
                        }
                        out.extend(n.decor);
                        out.extend(n.popup);
                        stack.extend(n.children);
                    }
                }
                out
            };
            for e in removed {
                if let Ok(em) = world.get_entity_mut(e) {
                    em.despawn();
                }
            }
            world.resource_mut::<UiTree>().mark_dirty(parent);
        }
        Op::Listen { .. } => {}
    }
}

/// Focus from `InputFocus`, and the env from the window, into the states.
pub(crate) fn sync_states(world: &mut World) {
    let focused = world.get_resource::<InputFocus>().and_then(|f| f.get());
    let focused_node = focused.and_then(|e| world.resource::<UiTree>().node_of(e));
    // Env: dark/coarse are the host's to set; size follows the primary window.
    let size = world
        .query_filtered::<&Window, With<bevy::window::PrimaryWindow>>()
        .single(world)
        .map(|w| (w.width(), w.height()))
        .ok();
    let scale = world
        .get_resource::<bevy::ui::UiScale>()
        .map_or(1.0, |s| s.0);
    let host_key = with_host(|host| {
        if let Some((w, h)) = size {
            host.env.viewport_width = w / scale;
            host.env.viewport_height = h / scale;
        }
        (
            host.env.dark,
            host.env.coarse_pointer,
            host.env.viewport_width as i32,
            host.env.viewport_height as i32,
            host.env.container_width.map_or(-1, |c| c as i32),
        )
    });
    let mut tree = world.resource_mut::<UiTree>();
    if tree.env_key != Some(host_key) {
        tree.env_key = Some(host_key);
        tree.mark_dirty(ROOT);
        tree.restyle_subtree(ROOT);
    }
    // Focus: the focused node and its ancestors (focus-within) change. Both
    // walks below visit every node, so only when the focus moved, or nodes
    // were inserted or moved that have not seen it yet.
    let focus_key = (focused_node, tree.generation);
    if tree.focus_key == Some(focus_key) {
        return;
    }
    tree.focus_key = Some(focus_key);
    let mut changed = Vec::new();
    for (id, n) in tree.nodes.iter_mut() {
        if let NodeKind::Element { states, .. } = &mut n.kind {
            let is_focus = Some(*id) == focused_node;
            if states.focus != is_focus {
                states.focus = is_focus;
                states.focus_visible = is_focus;
                changed.push(*id);
            }
        }
    }
    // focus-within on ancestors.
    let mut within: HashSet<NodeId> = HashSet::new();
    if let Some(f) = focused_node {
        let mut cur = tree.nodes.get(&f).and_then(|n| n.parent);
        while let Some(p) = cur {
            within.insert(p);
            cur = tree.nodes.get(&p).and_then(|n| n.parent);
        }
    }
    for (id, n) in tree.nodes.iter_mut() {
        if let NodeKind::Element { states, .. } = &mut n.kind {
            let w = within.contains(id);
            if states.focus_within != w {
                states.focus_within = w;
                changed.push(*id);
            }
        }
    }
    for id in changed {
        tree.restyle_subtree(id);
    }
}

/// Recomputes the style of every dirty node, parents before children, and
/// writes the components. Returns how many it styled.
pub(crate) fn restyle(world: &mut World) -> usize {
    if !world
        .resource::<UiTree>()
        .nodes
        .values()
        .any(|n| n.dirty || n.restyle)
    {
        return 0;
    }
    let mut styled = 0;
    // Order: depth first from the root, so a parent's computed is ready;
    // with depths, so the ancestors are a stack rather than a walk up from
    // every node.
    let order: Vec<(NodeId, usize)> = {
        let tree = world.resource::<UiTree>();
        let mut out = Vec::with_capacity(tree.nodes.len());
        let mut stack = vec![(ROOT, 0)];
        while let Some((id, depth)) = stack.pop() {
            out.push((id, depth));
            if let Some(n) = tree.nodes.get(&id) {
                for c in n.children.iter().rev() {
                    stack.push((*c, depth + 1));
                }
            }
        }
        out
    };
    let fonts = world.get_resource::<FontMap>().cloned();
    // The ancestors of the node in hand, below the root, as the cascade
    // sees them: built when a node under them first needs its chain.
    let mut path: Vec<(NodeId, Option<Element>)> = Vec::new();
    for (id, depth) in order {
        path.truncate(depth.saturating_sub(1));
        let (dirty, restyle) = world
            .resource::<UiTree>()
            .nodes
            .get(&id)
            .map_or((false, false), |n| (n.dirty, n.restyle));
        if id != ROOT {
            path.push((id, None));
        }
        if !dirty && !restyle {
            continue;
        }
        if id == ROOT {
            let root_computed = with_host(|host| host.stylist.root(&host.env));
            let mut tree = world.resource_mut::<UiTree>();
            let changed =
                tree.nodes.get(&ROOT).and_then(|n| n.computed.as_ref()) != Some(&root_computed);
            if changed {
                for c in tree
                    .nodes
                    .get(&ROOT)
                    .map(|n| n.children.clone())
                    .unwrap_or_default()
                {
                    tree.mark_restyle(c);
                }
            }
            if let Some(n) = tree.nodes.get_mut(&ROOT) {
                n.computed = Some(root_computed);
                n.dirty = false;
                n.restyle = false;
                styled += 1;
            }
            continue;
        }
        // Style it, borrowing the parent's style where it lies.
        let styled_node = {
            let tree = world.resource::<UiTree>();
            let ancestors = &path[..path.len() - 1];
            // Fill the ancestors' elements this chain needs.
            let missing: Vec<(usize, Element)> = ancestors
                .iter()
                .enumerate()
                .filter(|(_, (_, e))| e.is_none())
                .filter_map(|(i, (a, _))| Some((i, element_of(tree.nodes.get(a)?))))
                .collect();
            for (i, e) in missing {
                path[i].1 = Some(e);
            }
            let ancestors = &path[..path.len() - 1];
            style_node(tree, id, ancestors)
        };
        let Some((computed, opacity, states, entity)) = styled_node else {
            continue;
        };
        // Write the components when the view changed the node, or the
        // style came out different; a restyle that changed nothing writes
        // nothing.
        let (old_same, children) = {
            let tree = world.resource::<UiTree>();
            let n = tree.nodes.get(&id);
            let same = n.is_some_and(|n| {
                n.computed.as_ref() == Some(&computed)
                    && n.opacity == opacity
                    && n.written_states == states
            });
            let children = if n.and_then(|n| n.computed.as_ref()) != Some(&computed)
                || n.is_some_and(|n| n.opacity != opacity)
            {
                n.map(|n| n.children.clone()).unwrap_or_default()
            } else {
                Vec::new()
            };
            (same, children)
        };
        if dirty || !old_same {
            if let Some(e) = entity {
                write_components(world, e, id, &computed, opacity, fonts.as_ref());
            }
        }
        let mut tree = world.resource_mut::<UiTree>();
        // What inherits from it follows.
        for c in children {
            tree.mark_restyle(c);
        }
        if let Some(n) = tree.nodes.get_mut(&id) {
            n.computed = Some(computed);
            n.opacity = opacity;
            n.written_states = states;
            n.dirty = false;
            n.restyle = false;
            styled += 1;
        }
    }
    styled
}

/// The style of node `id` under `ancestors` (root first, below the root),
/// with its opacity, its states and its entity; `None` when it has no
/// styled parent yet.
fn style_node(
    tree: &UiTree,
    id: NodeId,
    ancestors: &[(NodeId, Option<Element>)],
) -> Option<(Computed, f32, Option<States>, Option<Entity>)> {
    let node = tree.nodes.get(&id)?;
    let parent = tree.nodes.get(&node.parent?)?;
    let parent_computed = parent.computed.as_ref()?;
    let states = match &node.kind {
        NodeKind::Element { states, .. } => Some(*states),
        _ => None,
    };
    let computed = if matches!(node.kind, NodeKind::Text(_)) {
        // Text inherits; nothing matches it.
        Computed {
            display: Display::Inline,
            ..inherit_only(parent_computed)
        }
    } else {
        // The previous *element* sibling, skipping anchors and text.
        let i = parent.children.iter().position(|c| *c == id)?;
        let previous = parent.children[..i].iter().rev().find_map(|s| {
            let sn = tree.nodes.get(s)?;
            matches!(sn.kind, NodeKind::Element { .. }).then(|| element_of(sn))
        });
        let refs: Vec<&Element> = ancestors.iter().filter_map(|(_, e)| e.as_ref()).collect();
        let element = element_of(node);
        let chain = Chain {
            ancestors: &refs,
            element: &element,
            previous_sibling: previous.as_ref(),
        };
        with_host(|host| host.stylist.compute(&chain, parent_computed, &host.env))
    };
    let computed = inherit_pointer_events(computed, parent_computed);
    let opacity = parent.opacity * computed.opacity;
    Some((computed, opacity, states, node.entity))
}

fn inherit_only(parent: &Computed) -> Computed {
    Computed {
        color: parent.color,
        font_size: parent.font_size,
        font_weight: parent.font_weight,
        font_family: parent.font_family.clone(),
        line_height: parent.line_height,
        letter_spacing: parent.letter_spacing,
        text_align: parent.text_align,
        nowrap: parent.nowrap,
        ellipsis: parent.ellipsis,
        uppercase: parent.uppercase,
        visible: parent.visible,
        custom: parent.custom.clone(),
        ..Default::default()
    }
}

/// `pointer-events` is inherited in CSS, and the cascade keeps it in
/// `other`, which is not: a node that does not say takes its parent's.
fn inherit_pointer_events(mut c: Computed, parent: &Computed) -> Computed {
    if !c.other.contains_key(POINTER_EVENTS) {
        if let Some(v) = parent.other.get(POINTER_EVENTS) {
            c.other.insert(POINTER_EVENTS.into(), v.clone());
        }
    }
    c
}

const POINTER_EVENTS: &str = "pointer-events";

/// What a press does over the node. Bevy's picking stops at the first node
/// under the pointer, so a full-window layer that does not listen covers
/// every screen under it unless `pointer-events: none` lets the press
/// through, as a browser does.
fn pickable(c: &Computed) -> Pickable {
    if c.other.get(POINTER_EVENTS).is_some_and(|v| v == "none") {
        Pickable::IGNORE
    } else {
        Pickable::default()
    }
}

fn element_of(n: &ShadowNode) -> Element {
    match &n.kind {
        NodeKind::Element {
            tag,
            classes,
            attrs,
            states,
            props,
            ..
        } => {
            let mut e = Element::new(tag);
            e.classes = classes.clone();
            e.attrs = attrs.clone();
            // `disabled` is an attribute; `checked` a property, mirrored in
            // the states already.
            let mut states = *states;
            states.disabled = attrs.iter().any(|(n, _)| n == "disabled");
            let _ = props;
            e.states = states;
            e
        }
        _ => Element::new("#text"),
    }
}

fn colour(c: Rgba, opacity: f32) -> Color {
    Color::srgba(c.r, c.g, c.b, c.a * opacity)
}

fn val(l: Length) -> Val {
    match l {
        Length::Px(v) => Val::Px(v),
        Length::Percent(p) => Val::Percent(p),
        Length::Auto => Val::Auto,
    }
}

fn rect(r: mithril::style::cascade::Rect) -> UiRect {
    UiRect {
        left: Val::Px(r.left),
        right: Val::Px(r.right),
        top: Val::Px(r.top),
        bottom: Val::Px(r.bottom),
    }
}

/// Parses a `rotate`/`translate`/`scale`/`transform` declaration into a
/// transform.
fn ui_transform(c: &Computed) -> Option<UiTransform> {
    let mut t = UiTransform::default();
    let mut any = false;
    // A pressed button shrinks a little (`scale: 0.97`); one number scales
    // both axes, two scale them apart.
    if let Some(s) = c.other.get("scale") {
        let parts: Vec<f32> = s
            .split_whitespace()
            .filter_map(|p| p.parse::<f32>().ok())
            .collect();
        if let Some(x) = parts.first() {
            t.scale = Vec2::new(*x, parts.get(1).copied().unwrap_or(*x));
            any = true;
        }
    }
    if let Some(r) = c.other.get("rotate") {
        if let Some(deg) = r
            .trim()
            .strip_suffix("deg")
            .and_then(|v| v.trim().parse::<f32>().ok())
        {
            t.rotation = Rot2::degrees(deg);
            any = true;
        }
    }
    if let Some(tr) = c.other.get("translate") {
        let parts: Vec<f32> = tr
            .split_whitespace()
            .map(|p| p.trim_end_matches("px").parse::<f32>().unwrap_or(0.0))
            .collect();
        if let Some(x) = parts.first() {
            t.translation = Val2::new(Val::Px(*x), Val::Px(parts.get(1).copied().unwrap_or(0.0)));
            any = true;
        }
    }
    if let Some(tf) = c.other.get("transform") {
        if let Some(inner) = tf.strip_prefix("rotate(").and_then(|r| r.strip_suffix(')')) {
            if let Some(deg) = inner
                .trim()
                .strip_suffix("deg")
                .and_then(|v| v.parse::<f32>().ok())
            {
                t.rotation = Rot2::degrees(deg);
                any = true;
            }
        }
        if let Some(inner) = tf
            .strip_prefix("translateY(")
            .and_then(|r| r.strip_suffix(')'))
        {
            if let Some(px) = inner
                .trim()
                .strip_suffix("px")
                .and_then(|v| v.parse::<f32>().ok())
            {
                t.translation = Val2::new(Val::Px(0.0), Val::Px(px));
                any = true;
            }
        }
    }
    any.then_some(t)
}

fn write_components(
    world: &mut World,
    e: Entity,
    id: NodeId,
    c: &Computed,
    opacity: f32,
    fonts: Option<&FontMap>,
) {
    let is_text = world.get::<Text>(e).is_some();
    world.entity_mut(e).insert(pickable(c));
    let (tag, classes, props, states) = {
        let tree = world.resource::<UiTree>();
        match tree.nodes.get(&id).map(|n| &n.kind) {
            Some(NodeKind::Element {
                tag,
                classes,
                props,
                states,
                ..
            }) => (tag.clone(), classes.clone(), props.clone(), *states),
            _ => (String::new(), Vec::new(), HashMap::new(), States::default()),
        }
    };
    if is_text {
        let text_font = fonts.map(|f| TextFont {
            font: f.pick(&c.font_family, c.font_weight).into(),
            font_size: bevy::text::FontSize::Px(f.size(&c.font_family, c.font_size)),
            ..default()
        });
        let mut em = world.entity_mut(e);
        if let Some(tf) = text_font {
            em.insert(tf);
        } else {
            em.insert(TextFont {
                font_size: bevy::text::FontSize::Px(c.font_size),
                ..default()
            });
        }
        em.insert(TextColor(colour(c.color, opacity)));
        em.insert(LineHeight::RelativeToFont(c.line_height));
        // A line the browser ends in "…" stays one line here too, may
        // shrink to its box, and is cut to fit after the layout
        // (`crate::ellipsis`): Bevy's text has no ellipsis of its own.
        let fit = c.nowrap && c.ellipsis;
        let mut layout = if c.nowrap {
            TextLayout::no_wrap()
        } else {
            TextLayout::default()
        };
        layout.justify = match c.text_align {
            TextAlign::Center => Justify::Center,
            TextAlign::End => Justify::Right,
            TextAlign::Start => Justify::Left,
        };
        em.insert(layout);
        if c.uppercase {
            if let Some(mut t) = em.get_mut::<Text>() {
                let up = t.0.to_uppercase();
                if t.0 != up {
                    t.0 = up;
                }
            }
        }
        // A line that may be cut takes the room its line has, not its own
        // width: sized by its text, a cut line would shrink its box to
        // itself and never see the room come back.
        // Restyled, a line being cut keeps asking for its whole width.
        let asked = em
            .get::<crate::ellipsis::Ellipsis>()
            .and_then(|el| el.full_width)
            .filter(|_| fit)
            .map_or(Val::Auto, Val::Px);
        em.insert(Node {
            flex_shrink: 1.0,
            flex_grow: if fit { 1.0 } else { 0.0 },
            min_width: if fit { Val::Px(0.0) } else { Val::Auto },
            width: asked,
            ..default()
        });
        if fit {
            if em.get::<crate::ellipsis::Ellipsis>().is_none() {
                let full = em.get::<Text>().map(|t| t.0.clone()).unwrap_or_default();
                em.insert(crate::ellipsis::Ellipsis::new(full));
            }
        } else {
            em.remove::<crate::ellipsis::Ellipsis>();
        }
        return;
    }
    let is_input = tag == "input";
    let input_type = props_or_attr(&id, world, "type").unwrap_or_default();
    let drawn = is_input && matches!(input_type.as_str(), "checkbox" | "radio");
    let range = is_input && input_type == "range";
    let text_input = (is_input && !drawn && !range) || tag == "textarea";
    let mut node = Node {
        display: match c.display {
            Display::None => bevy::ui::Display::None,
            Display::Grid | Display::InlineGrid => bevy::ui::Display::Grid,
            Display::Block if !classes.is_empty() || tag == "div" => bevy::ui::Display::Block,
            _ => bevy::ui::Display::Flex,
        },
        flex_direction: match c.flex_direction {
            mithril::style::cascade::Direction::Row => FlexDirection::Row,
            mithril::style::cascade::Direction::Column => FlexDirection::Column,
        },
        flex_wrap: if c.flex_wrap {
            FlexWrap::Wrap
        } else {
            FlexWrap::NoWrap
        },
        align_items: match c.align_items {
            mithril::style::cascade::Align::Stretch => AlignItems::Stretch,
            mithril::style::cascade::Align::Start => AlignItems::FlexStart,
            mithril::style::cascade::Align::Center => AlignItems::Center,
            mithril::style::cascade::Align::End => AlignItems::FlexEnd,
            mithril::style::cascade::Align::Baseline => AlignItems::Baseline,
        },
        align_self: match c.align_self {
            None => AlignSelf::Auto,
            Some(mithril::style::cascade::Align::Stretch) => AlignSelf::Stretch,
            Some(mithril::style::cascade::Align::Start) => AlignSelf::FlexStart,
            Some(mithril::style::cascade::Align::Center) => AlignSelf::Center,
            Some(mithril::style::cascade::Align::End) => AlignSelf::FlexEnd,
            Some(mithril::style::cascade::Align::Baseline) => AlignSelf::Baseline,
        },
        justify_content: match c.justify_content {
            mithril::style::cascade::Justify::Start => JustifyContent::FlexStart,
            mithril::style::cascade::Justify::Center => JustifyContent::Center,
            mithril::style::cascade::Justify::End => JustifyContent::FlexEnd,
            mithril::style::cascade::Justify::SpaceBetween => JustifyContent::SpaceBetween,
            mithril::style::cascade::Justify::SpaceAround => JustifyContent::SpaceAround,
            mithril::style::cascade::Justify::SpaceEvenly => JustifyContent::SpaceEvenly,
        },
        flex_grow: c.flex_grow,
        flex_shrink: c.flex_shrink,
        flex_basis: c.flex_basis.map_or(Val::Auto, val),
        row_gap: Val::Px(c.row_gap),
        column_gap: Val::Px(c.column_gap),
        padding: rect(c.padding),
        margin: {
            let mut m = rect(c.margin);
            if c.margin_auto[0] {
                m.top = Val::Auto;
            }
            if c.margin_auto[1] {
                m.right = Val::Auto;
            }
            if c.margin_auto[2] {
                m.bottom = Val::Auto;
            }
            if c.margin_auto[3] {
                m.left = Val::Auto;
            }
            m
        },
        border: rect(c.border),
        border_radius: BorderRadius {
            top_left: Val::Px(c.border_radius[0]).into(),
            top_right: Val::Px(c.border_radius[1]).into(),
            bottom_right: Val::Px(c.border_radius[2]).into(),
            bottom_left: Val::Px(c.border_radius[3]).into(),
        },
        width: val(c.width),
        height: val(c.height),
        min_width: val(c.min_width),
        min_height: val(c.min_height),
        max_width: val(c.max_width),
        max_height: val(c.max_height),
        position_type: match c.position {
            Position::Absolute | Position::Fixed => PositionType::Absolute,
            _ => PositionType::Relative,
        },
        left: c.inset.left.map_or(Val::Auto, val),
        right: c.inset.right.map_or(Val::Auto, val),
        top: c.inset.top.map_or(Val::Auto, val),
        bottom: c.inset.bottom.map_or(Val::Auto, val),
        overflow: Overflow {
            x: overflow_axis(c.overflow_x),
            y: overflow_axis(c.overflow_y),
        },
        grid_template_columns: grid_tracks(c.grid_template_columns.as_deref()),
        grid_template_rows: grid_tracks(c.grid_template_rows.as_deref()),
        ..default()
    };
    // A select's options are not laid out: the select shows the chosen
    // one (a decoration) and the rest come up in a popup.
    if tag == "option" {
        node.display = bevy::ui::Display::None;
    }
    if tag == "select" {
        node.align_items = AlignItems::Center;
    }
    // A block holding text beside an element (a label and its badge) is a
    // line of inline content in a browser. Bevy's block layout stretches
    // every child to the full width instead, so it becomes a column whose
    // items keep their own width: the badge sits under the label. (A
    // wrapping row measured a wrapped label one line short, and the badge
    // fell out of the tile.)
    if node.display == bevy::ui::Display::Block && mixed_content(world, id) {
        node.display = bevy::ui::Display::Flex;
        node.flex_direction = FlexDirection::Column;
        node.align_items = AlignItems::FlexStart;
    }
    // An inline element is a row whose items sit on the baseline.
    if matches!(c.display, Display::Inline) {
        node.display = bevy::ui::Display::Flex;
        node.align_items = AlignItems::Baseline;
    }
    // A text input's own box grows in its well.
    if text_input {
        node.flex_grow = 1.0;
        node.min_width = Val::Px(0.0);
    }
    // An `<img>`: its `data:` source decoded and shared while shown
    // (`images.rs`).
    let picture = if tag == "img" {
        let src = props_or_attr(&id, world, "src");
        crate::images::picture(world, id, src.as_deref())
    } else {
        None
    };
    // A drawn control has its size from the sheet already.
    let mut em = world.entity_mut(e);
    em.insert(node);
    match picture {
        Some(handle) => {
            em.insert(ImageNode::new(handle));
        }
        None => {
            em.remove::<ImageNode>();
        }
    }
    em.insert(BackgroundColor(
        c.background.map_or(Color::NONE, |b| colour(b, opacity)),
    ));
    let gradient = ["background-image", "background"]
        .iter()
        .find_map(|p| c.other.get(*p))
        .and_then(|v| crate::gradients::parse(v, opacity));
    match gradient {
        Some(g) => {
            em.insert(BackgroundGradient(g));
        }
        None => {
            em.remove::<BackgroundGradient>();
        }
    }
    let side = |b: Option<Rgba>| b.map_or(Color::NONE, |b| colour(b, opacity));
    em.insert(BorderColor {
        top: side(c.border_top_color.or(c.border_color)),
        right: side(c.border_color),
        bottom: side(c.border_bottom_color.or(c.border_color)),
        left: side(c.border_color),
    });
    // The one animation the sheet has: a spinner's turn.
    if c.other
        .get("animation")
        .is_some_and(|a| a.contains("m-spin"))
    {
        em.insert(crate::Spin);
    } else {
        em.remove::<crate::Spin>();
    }
    if c.box_shadow.is_empty() {
        em.remove::<BoxShadow>();
    } else {
        em.insert(BoxShadow(
            c.box_shadow
                .iter()
                .filter(|s| !s.inset)
                .map(|s| ShadowStyle {
                    color: colour(s.color, opacity),
                    x_offset: Val::Px(s.x),
                    y_offset: Val::Px(s.y),
                    spread_radius: Val::Px(s.spread),
                    // CSS's blur is twice the Gaussian's deviation; Bevy's
                    // `blur_radius` is the deviation itself (its shadow reaches
                    // three of them out), so CSS's 6 px came out as 12.
                    blur_radius: Val::Px(s.blur / 2.0),
                })
                .collect(),
        ));
    }
    match c.z_index {
        Some(z) if matches!(c.position, Position::Fixed) => {
            em.insert(GlobalZIndex(z));
        }
        Some(z) => {
            em.insert(ZIndex(z));
        }
        None => {
            em.insert(ZIndex(0));
        }
    }
    em.insert(if c.visible {
        Visibility::Inherited
    } else {
        Visibility::Hidden
    });
    // Never removed: the layout walk reads `UiTransform` on every node and
    // stops at one without it, which leaves its whole subtree 0×0 at (0,0).
    em.insert(ui_transform(c).unwrap_or(UiTransform::IDENTITY));
    if c.overflow_y == Ov::Scroll || c.overflow_y == Ov::Auto {
        if em.get::<ScrollArea>().is_none() {
            // The widgets' scroll area reads the wheel; a finger's drag is
            // ours, as the drag's delta against the scroll position.
            em.insert(ScrollArea);
            em.observe(
                |ev: On<PointerDrag>,
                 mut areas: Query<(&ComputedNode, &mut ScrollPosition), With<ScrollArea>>| {
                    if let Ok((computed, mut pos)) = areas.get_mut(ev.entity) {
                        let scale = computed.inverse_scale_factor();
                        pos.y -= ev.delta.y * scale;
                        pos.x -= ev.delta.x * scale;
                    }
                },
            );
        }
    }
    // A range input is the widgets' headless slider: it turns a press or a
    // drag on the track into a `ValueChange` (`events.rs`); the track, the
    // fill and the thumb are decorations below.
    if range {
        let value = props_or_attr(&id, world, "value").or_else(|| props.get("value").cloned());
        let num = |s: Option<String>, d: f32| s.and_then(|v| v.parse::<f32>().ok()).unwrap_or(d);
        let min = num(props_or_attr(&id, world, "min"), 0.0);
        let max = num(props_or_attr(&id, world, "max"), 100.0);
        let step = num(props_or_attr(&id, world, "step"), 1.0);
        let mut em = world.entity_mut(e);
        if em.get::<Slider>().is_none() {
            em.insert(Slider {
                track_click: TrackClick::Snap,
                ..default()
            });
        }
        em.insert((
            SliderValue(num(value, min)),
            SliderRange::new(min, max),
            SliderStep(step),
        ));
    }
    let mut em = world.entity_mut(e);
    // The text controls: a `TextInput` over an `EditableText`.
    if text_input {
        let value = props.get("value").cloned().unwrap_or_default();
        if em.get::<TextInput>().is_none() {
            let mut editable = EditableText::new(&value);
            editable.allow_newlines = tag == "textarea";
            em.insert((TextInput, editable, TextLayout::no_wrap()));
        }
        em.insert(TextColor(colour(c.color, opacity)));
        let tf = match fonts {
            Some(f) => TextFont {
                font: f.pick(&c.font_family, c.font_weight).into(),
                font_size: bevy::text::FontSize::Px(f.size(&c.font_family, c.font_size)),
                ..default()
            },
            None => TextFont {
                font_size: bevy::text::FontSize::Px(c.font_size),
                ..default()
            },
        };
        em.insert(tf);
        em.insert(LineHeight::RelativeToFont(c.line_height));
    }
    // The drawn pseudo-elements.
    let ctx = {
        let num = |s: Option<String>, d: f64| s.and_then(|v| v.parse::<f64>().ok()).unwrap_or(d);
        let min = num(props_or_attr(&id, world, "min"), 0.0);
        let select_label = (tag == "select").then(|| {
            // The option at `selectedIndex`, the placeholder option counted.
            let index = props
                .get("selectedIndex")
                .and_then(|v| v.parse::<i64>().ok())
                .unwrap_or(0);
            let tree = world.resource::<UiTree>();
            let options: Vec<String> = tree
                .nodes
                .get(&id)
                .map(|n| {
                    n.children
                        .iter()
                        .filter_map(|c| tree.nodes.get(c))
                        .filter(|cn| matches!(&cn.kind, NodeKind::Element { tag, .. } if tag == "option"))
                        .map(|cn| option_label(tree, cn))
                        .collect()
                })
                .unwrap_or_default();
            usize::try_from(index)
                .ok()
                .and_then(|i| options.get(i).cloned())
                .unwrap_or_default()
        });
        DecorContext {
            value: num(
                props
                    .get("value")
                    .cloned()
                    .or_else(|| props_or_attr(&id, world, "value")),
                min,
            ),
            min,
            max: num(props_or_attr(&id, world, "max"), 100.0),
            placeholder: text_input
                .then(|| props_or_attr(&id, world, "placeholder"))
                .flatten(),
            select_label,
        }
    };
    let decor_specs = decorations(&tag, &classes, &input_type, c, opacity, states, &ctx);
    let old: Vec<Entity> = world
        .resource::<UiTree>()
        .nodes
        .get(&id)
        .map(|n| n.decor.clone())
        .unwrap_or_default();
    for d in old {
        if let Ok(em) = world.get_entity_mut(d) {
            em.despawn();
        }
    }
    let mut new_decor = Vec::new();
    for spec in decor_specs {
        let d = world
            .spawn((
                spec.node,
                BackgroundColor(spec.background),
                BorderColor::all(spec.border),
                spec.transform,
                Pickable::IGNORE,
                Name::new("decor"),
            ))
            .id();
        if let Some(bc) = spec.border_sides {
            world.entity_mut(d).insert(bc);
        }
        if let Some(shadow) = spec.shadow {
            world.entity_mut(d).insert(shadow);
        }
        if let Some((text, colour)) = spec.text {
            let tf = match fonts {
                Some(f) => TextFont {
                    font: f.pick(&c.font_family, c.font_weight).into(),
                    font_size: bevy::text::FontSize::Px(f.size(&c.font_family, c.font_size)),
                    ..default()
                },
                None => TextFont {
                    font_size: bevy::text::FontSize::Px(c.font_size),
                    ..default()
                },
            };
            world.entity_mut(d).insert((
                Text::new(text),
                tf,
                TextColor(colour),
                TextLayout::no_wrap(),
                LineHeight::RelativeToFont(c.line_height),
            ));
        }
        match spec.role {
            DecorRole::Plain => {}
            DecorRole::Thumb => {
                world.entity_mut(d).insert((SliderThumb, DecorRole::Thumb));
            }
            DecorRole::Placeholder => {
                world.entity_mut(d).insert(DecorRole::Placeholder);
            }
        }
        world.entity_mut(d).insert(ChildOf(e));
        new_decor.push(d);
    }
    // Decorations go first among the children so content draws over them.
    if !new_decor.is_empty() {
        let content: Vec<Entity> = {
            let tree = world.resource::<UiTree>();
            tree.nodes
                .get(&id)
                .map(|n| {
                    n.children
                        .iter()
                        .filter_map(|c| tree.nodes.get(c).and_then(|cn| cn.entity))
                        .collect()
                })
                .unwrap_or_default()
        };
        let mut all = new_decor.clone();
        all.extend(content);
        world.entity_mut(e).replace_children(&all);
    }
    if let Some(n) = world.resource_mut::<UiTree>().nodes.get_mut(&id) {
        n.decor = new_decor;
    }
}

/// Whether a node's children are text and elements both. (Inserting or
/// removing a child marks the parent dirty, so this is re-read then.)
fn mixed_content(world: &World, id: NodeId) -> bool {
    let tree = world.resource::<UiTree>();
    let Some(n) = tree.nodes.get(&id) else {
        return false;
    };
    let kinds = n.children.iter().filter_map(|c| tree.nodes.get(c));
    let (mut text, mut element) = (false, false);
    for k in kinds {
        match k.kind {
            NodeKind::Text(_) => text = true,
            NodeKind::Element { .. } => element = true,
            NodeKind::Anchor => {}
        }
    }
    text && element
}

fn props_or_attr(id: &NodeId, world: &World, name: &str) -> Option<String> {
    let tree = world.resource::<UiTree>();
    match tree.nodes.get(id).map(|n| &n.kind) {
        Some(NodeKind::Element { attrs, .. }) => attrs
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.clone()),
        _ => None,
    }
}

/// A `grid-template-*` as Bevy's tracks: the forms the sheets use —
/// `repeat(auto-fill, minmax(104px, 1fr))`, `repeat(3, 1fr)`, and a list of
/// `1fr`, `Npx`, `auto`. Anything else leaves the template empty (one
/// implicit track).
fn grid_tracks(template: Option<&str>) -> Vec<RepeatedGridTrack> {
    use bevy::ui::{GridTrackRepetition, MaxTrackSizingFunction, MinTrackSizingFunction};
    let Some(t) = template else {
        return Vec::new();
    };
    let t = t.trim();
    if let Some(inner) = t.strip_prefix("repeat(").and_then(|r| r.strip_suffix(')')) {
        let (count, tracks) = match inner.split_once(',') {
            Some(p) => p,
            None => return Vec::new(),
        };
        let repetition = match count.trim() {
            "auto-fill" => GridTrackRepetition::AutoFill,
            "auto-fit" => GridTrackRepetition::AutoFit,
            n => match n.parse::<u16>() {
                Ok(n) => GridTrackRepetition::Count(n),
                Err(_) => return Vec::new(),
            },
        };
        let tracks = tracks.trim();
        if let Some(mm) = tracks
            .strip_prefix("minmax(")
            .and_then(|r| r.strip_suffix(')'))
        {
            let Some((min, max)) = mm.split_once(',') else {
                return Vec::new();
            };
            let min = min.trim().trim_end_matches("px").parse::<f32>().ok();
            let max = max.trim();
            let max = if let Some(fr) = max.strip_suffix("fr") {
                fr.parse::<f32>().ok().map(MaxTrackSizingFunction::Fraction)
            } else if max == "auto" {
                Some(MaxTrackSizingFunction::Auto)
            } else {
                max.trim_end_matches("px")
                    .parse::<f32>()
                    .ok()
                    .map(MaxTrackSizingFunction::Px)
            };
            return match (min, max) {
                (Some(min), Some(max)) => vec![RepeatedGridTrack::minmax(
                    repetition,
                    MinTrackSizingFunction::Px(min),
                    max,
                )],
                _ => Vec::new(),
            };
        }
        if let Some(fr) = tracks.strip_suffix("fr") {
            return match (fr.parse::<f32>(), repetition) {
                (Ok(fr), GridTrackRepetition::Count(n)) => vec![RepeatedGridTrack::fr(n, fr)],
                _ => Vec::new(),
            };
        }
        return Vec::new();
    }
    t.split_whitespace()
        .filter_map(|part| {
            if part == "auto" {
                Some(GridTrack::auto())
            } else if let Some(fr) = part.strip_suffix("fr") {
                fr.parse::<f32>().ok().map(GridTrack::fr)
            } else {
                part.strip_suffix("px")
                    .and_then(|px| px.parse::<f32>().ok())
                    .map(GridTrack::px)
            }
        })
        .collect()
}

fn overflow_axis(o: Ov) -> OverflowAxis {
    match o {
        Ov::Visible => OverflowAxis::Visible,
        Ov::Hidden => OverflowAxis::Hidden,
        Ov::Scroll | Ov::Auto => OverflowAxis::Scroll,
    }
}

struct Decor {
    node: Node,
    background: Color,
    border: Color,
    border_sides: Option<BorderColor>,
    transform: UiTransform,
    shadow: Option<BoxShadow>,
    /// A text decoration (a select's shown label, an input's placeholder),
    /// in the element's font.
    text: Option<(String, Color)>,
    /// Which decoration is which, for the systems that move or hide one.
    role: DecorRole,
}

#[derive(Component, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum DecorRole {
    #[default]
    Plain,
    /// The slider's thumb: `bevy_ui_widgets` measures it to reduce the travel.
    Thumb,
    /// Shown only while the input is empty (`sync_inputs`).
    Placeholder,
}

/// What a decoration needs of the element beyond its style: the slider's
/// numbers, the input's placeholder, the select's chosen label.
#[derive(Default)]
struct DecorContext {
    value: f64,
    min: f64,
    max: f64,
    placeholder: Option<String>,
    select_label: Option<String>,
}

fn custom_colour(c: &Computed, name: &str, opacity: f32, fallback: Rgba) -> Color {
    let v = c
        .custom
        .get(name)
        .and_then(|v| Rgba::parse(v))
        .unwrap_or(fallback);
    colour(v, opacity)
}

/// What the stylesheet draws with `::before`/`::after`, drawn here from the
/// computed colours: the tick, the dot, the knob, the bars, the chevron,
/// the ring.
fn decorations(
    tag: &str,
    classes: &[String],
    input_type: &str,
    c: &Computed,
    opacity: f32,
    states: States,
    ctx: &DecorContext,
) -> Vec<Decor> {
    let has = |class: &str| classes.iter().any(|x| x == class);
    let mut out = Vec::new();
    let on_accent = custom_colour(c, "--m-on-accent", opacity, Rgba::rgb(0.06, 0.09, 0.12));
    let text_dim = custom_colour(c, "--m-text-dim", opacity, c.color);
    let text_faint = custom_colour(c, "--m-text-faint", opacity, c.color);
    let accent = custom_colour(c, "--m-accent", opacity, c.color);
    let border_colour = custom_colour(c, "--m-border", opacity, c.color);
    let current = colour(c.color, opacity);
    let plain = |n: Node, bg: Color| Decor {
        node: n,
        background: bg,
        border: Color::NONE,
        border_sides: None,
        transform: UiTransform::default(),
        shadow: None,
        text: None,
        role: DecorRole::Plain,
    };
    // A range input: the track, its filled part and the thumb, which the
    // sheet draws with the browser's slider pseudo-elements. The thumb sits
    // at `pct` of the travel, the travel being the width less the thumb's
    // (`left: pct%` pulled back by `pct` of its own width), which is how
    // `bevy_ui_widgets::Slider` measures a click or drag.
    if tag == "input" && input_type == "range" {
        let pct = if ctx.max > ctx.min {
            (((ctx.value - ctx.min) / (ctx.max - ctx.min)).clamp(0.0, 1.0) * 100.0) as f32
        } else {
            0.0
        };
        let bar = |width: Val, bg: Color| {
            plain(
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Px(0.0),
                    top: Val::Percent(50.0),
                    width,
                    height: Val::Px(6.0),
                    margin: UiRect::top(Val::Px(-3.0)),
                    border_radius: BorderRadius::all(Val::Px(3.0)),
                    ..default()
                },
                bg,
            )
        };
        out.push(bar(Val::Percent(100.0), border_colour));
        out.push(bar(Val::Percent(pct), accent));
        out.push(Decor {
            node: Node {
                position_type: PositionType::Absolute,
                left: Val::Percent(pct),
                top: Val::Percent(50.0),
                width: Val::Px(20.0),
                height: Val::Px(20.0),
                margin: UiRect::top(Val::Px(-10.0)),
                border: UiRect::all(Val::Px(2.0)),
                border_radius: BorderRadius::all(Val::Px(10.0)),
                ..default()
            },
            background: Color::srgba(1.0, 1.0, 1.0, opacity),
            border: accent,
            border_sides: None,
            transform: UiTransform::from_translation(Val2::new(Val::Percent(-pct), Val::Px(0.0))),
            shadow: Some(BoxShadow(vec![ShadowStyle {
                color: Color::srgba(0.0, 0.0, 0.0, 0.25 * opacity),
                x_offset: Val::Px(0.0),
                y_offset: Val::Px(1.0),
                spread_radius: Val::Px(0.0),
                blur_radius: Val::Px(3.0),
            }])),
            text: None,
            role: DecorRole::Thumb,
        });
    }
    // A text input's placeholder, which bevy_text does not draw yet.
    if let Some(p) = &ctx.placeholder {
        out.push(Decor {
            node: Node {
                position_type: PositionType::Absolute,
                left: Val::Px(c.padding.left + c.border.left),
                top: Val::Percent(50.0),
                ..default()
            },
            background: Color::NONE,
            border: Color::NONE,
            border_sides: None,
            transform: UiTransform::from_translation(Val2::new(Val::Px(0.0), Val::Percent(-50.0))),
            shadow: None,
            text: Some((p.clone(), text_faint)),
            role: DecorRole::Placeholder,
        });
    }
    // A select shows its chosen option; the options themselves are hidden
    // and come up in a popup (`lib.rs`, `open_select`).
    if tag == "select" {
        out.push(Decor {
            node: Node::default(),
            background: Color::NONE,
            border: Color::NONE,
            border_sides: None,
            transform: UiTransform::default(),
            shadow: None,
            text: Some((ctx.select_label.clone().unwrap_or_default(), current)),
            role: DecorRole::Plain,
        });
    }
    if tag == "input" && input_type == "checkbox" && has("m-checkbox") {
        if states.indeterminate {
            out.push(plain(
                Node {
                    position_type: PositionType::Absolute,
                    width: Val::Px(10.0),
                    height: Val::Px(2.0),
                    left: Val::Percent(50.0),
                    top: Val::Percent(50.0),
                    ..default()
                },
                on_accent,
            ));
            if let Some(d) = out.last_mut() {
                d.transform = UiTransform::from_translation(Val2::new(
                    Val::Percent(-50.0),
                    Val::Percent(-50.0),
                ));
            }
        } else if states.checked {
            out.push(Decor {
                node: Node {
                    position_type: PositionType::Absolute,
                    width: Val::Px(5.0),
                    height: Val::Px(10.0),
                    left: Val::Percent(50.0),
                    top: Val::Percent(50.0),
                    border: UiRect {
                        right: Val::Px(2.0),
                        bottom: Val::Px(2.0),
                        ..UiRect::ZERO
                    },
                    ..default()
                },
                background: Color::NONE,
                border: on_accent,
                border_sides: None,
                transform: UiTransform {
                    translation: Val2::new(Val::Percent(-50.0), Val::Px(-7.0)),
                    rotation: Rot2::degrees(45.0),
                    ..default()
                },
                shadow: None,
                text: None,
                role: DecorRole::Plain,
            });
        }
    }
    if tag == "input" && input_type == "radio" && has("m-radio") && states.checked {
        out.push(plain(
            Node {
                position_type: PositionType::Absolute,
                width: Val::Px(8.0),
                height: Val::Px(8.0),
                left: Val::Percent(50.0),
                top: Val::Percent(50.0),
                border_radius: BorderRadius::all(Val::Px(4.0)),
                ..default()
            },
            on_accent,
        ));
        if let Some(d) = out.last_mut() {
            d.transform =
                UiTransform::from_translation(Val2::new(Val::Percent(-50.0), Val::Percent(-50.0)));
        }
    }
    if tag == "input" && input_type == "checkbox" && has("m-switch") {
        out.push(Decor {
            node: Node {
                position_type: PositionType::Absolute,
                top: Val::Px(3.0),
                left: Val::Px(if states.checked { 19.0 } else { 3.0 }),
                width: Val::Px(16.0),
                height: Val::Px(16.0),
                border_radius: BorderRadius::all(Val::Px(8.0)),
                ..default()
            },
            background: Color::srgba(1.0, 1.0, 1.0, opacity),
            border: Color::NONE,
            border_sides: None,
            transform: UiTransform::default(),
            shadow: Some(BoxShadow(vec![ShadowStyle {
                color: Color::srgba(0.0, 0.0, 0.0, 0.3 * opacity),
                x_offset: Val::Px(0.0),
                y_offset: Val::Px(1.0),
                spread_radius: Val::Px(0.0),
                blur_radius: Val::Px(3.0),
            }])),
            text: None,
            role: DecorRole::Plain,
        });
    }
    // A tile's drawn switch: the knob at `--m-knob`, which the sheet moves
    // with `[data-on]` (the same property its `::after` reads).
    if has("m-tile__switch") {
        let knob = c
            .custom
            .get("--m-knob")
            .and_then(|v| v.trim().trim_end_matches("px").parse::<f32>().ok())
            .unwrap_or(2.0);
        out.push(plain(
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(2.0),
                left: Val::Px(knob),
                width: Val::Px(10.0),
                height: Val::Px(10.0),
                border_radius: BorderRadius::all(Val::Px(5.0)),
                ..default()
            },
            Color::srgba(1.0, 1.0, 1.0, opacity),
        ));
    }
    if has("m-close") {
        for deg in [45.0, -45.0] {
            out.push(Decor {
                node: Node {
                    position_type: PositionType::Absolute,
                    left: Val::Percent(50.0),
                    top: Val::Percent(50.0),
                    width: Val::Px(14.0),
                    height: Val::Px(2.0),
                    border_radius: BorderRadius::all(Val::Px(1.0)),
                    ..default()
                },
                background: current,
                border: Color::NONE,
                border_sides: None,
                transform: UiTransform {
                    translation: Val2::new(Val::Percent(-50.0), Val::Percent(-50.0)),
                    rotation: Rot2::degrees(deg),
                    ..default()
                },
                shadow: None,
                text: None,
                role: DecorRole::Plain,
            });
        }
    }
    if has("m-select") {
        out.push(Decor {
            node: Node {
                position_type: PositionType::Absolute,
                right: Val::Px(14.0),
                top: Val::Percent(50.0),
                width: Val::Px(7.0),
                height: Val::Px(7.0),
                margin: UiRect::top(Val::Px(-6.0)),
                border: UiRect {
                    right: Val::Px(2.0),
                    bottom: Val::Px(2.0),
                    ..UiRect::ZERO
                },
                ..default()
            },
            background: Color::NONE,
            border: text_dim,
            border_sides: None,
            transform: UiTransform::from_rotation(Rot2::degrees(45.0)),
            shadow: None,
            text: None,
            role: DecorRole::Plain,
        });
    }
    if has("m-spinner") {
        // The ring is the element's own border; only its top differs, and
        // per-side colours are the element's. Nothing to add here: the
        // caller sets a per-side BorderColor through `spinner_border`.
    }
    out
}

/// Keeps a text control's text equal to its `value` property when the
/// model changed it from outside (not while the person types in it).
pub(crate) fn sync_inputs(world: &mut World) {
    let focused = world.get_resource::<InputFocus>().and_then(|f| f.get());
    // Every text control and range: its entity, its `value` property and
    // its decorations.
    let targets: Vec<(Entity, Option<String>, Vec<Entity>)> = {
        let tree = world.resource::<UiTree>();
        tree.nodes
            .values()
            .filter_map(|n| match (&n.kind, n.entity) {
                (NodeKind::Element { props, tag, .. }, Some(e))
                    if (tag == "input" || tag == "textarea") =>
                {
                    Some((e, props.get("value").cloned(), n.decor.clone()))
                }
                _ => None,
            })
            .collect()
    };
    for (e, value, decor) in targets {
        // A range: the widget's value follows the model's.
        if let Some(v) = value
            .as_deref()
            .and_then(|v| v.parse::<f32>().ok())
            .filter(|_| world.get::<Slider>(e).is_some())
        {
            if world.get::<SliderValue>(e).is_some_and(|s| s.0 != v) {
                world.entity_mut(e).insert(SliderValue(v));
            }
            continue;
        }
        if Some(e) != focused {
            if let (Some(value), Some(mut editable)) =
                (value.as_deref(), world.get_mut::<EditableText>(e))
            {
                if editable.editor.text() != value {
                    editable.editor.set_text(value);
                    editable.queue_edit(bevy::text::TextEdit::TextEnd(false));
                }
            }
        }
        // The placeholder shows while the control is empty — by what is
        // typed, not only by the model, so it goes at the first key.
        let empty = world
            .get::<EditableText>(e)
            .is_some_and(|ed| ed.editor.text().to_string().is_empty());
        for d in decor {
            if world.get::<DecorRole>(d) == Some(&DecorRole::Placeholder) {
                let want = if empty {
                    Visibility::Inherited
                } else {
                    Visibility::Hidden
                };
                if world.get::<Visibility>(d) != Some(&want) {
                    world.entity_mut(d).insert(want);
                }
            }
        }
    }
}

/// An option's text, for the select's shown label.
fn option_label(tree: &UiTree, option: &ShadowNode) -> String {
    option
        .children
        .iter()
        .filter_map(|c| tree.nodes.get(c))
        .filter_map(|n| match &n.kind {
            NodeKind::Text(t) => Some(t.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

/// A select's options, for its popup: the label and whether it is disabled,
/// in order, the placeholder option counted.
pub(crate) fn select_options(tree: &UiTree, id: NodeId) -> Vec<(String, bool)> {
    tree.nodes
        .get(&id)
        .map(|n| {
            n.children
                .iter()
                .filter_map(|c| tree.nodes.get(c))
                .filter_map(|cn| match &cn.kind {
                    NodeKind::Element { tag, attrs, .. } if tag == "option" => Some((
                        option_label(tree, cn),
                        attrs.iter().any(|(n, _)| n == "disabled"),
                    )),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The tree as text, one line a node: for `MITHRIL_BEVY_DUMP=1`, which logs
/// it after every restyle.
pub(crate) fn dump(world: &World, limit: usize) -> String {
    use std::fmt::Write as _;
    let tree = world.resource::<UiTree>();
    let mut out = String::new();
    let mut stack = vec![(ROOT, 0usize)];
    let mut lines = 0;
    while let Some((id, depth)) = stack.pop() {
        let Some(n) = tree.nodes.get(&id) else {
            continue;
        };
        if lines >= limit {
            break;
        }
        lines += 1;
        let indent = "  ".repeat(depth);
        let what = match &n.kind {
            NodeKind::Element { tag, classes, .. } => format!("<{tag} .{}>", classes.join(".")),
            NodeKind::Text(t) => format!("{t:?}"),
            NodeKind::Anchor => "#anchor".to_owned(),
        };
        let _ = write!(out, "\n{indent}{what}");
        if let Some(e) = n.entity {
            let size = world.get::<ComputedNode>(e).map(|c| c.size());
            let node = world.get::<Node>(e);
            let bg = world.get::<BackgroundColor>(e).map(|b| b.0.to_srgba());
            let vis = world.get::<Visibility>(e).copied();
            let children = world.get::<Children>(e).map_or(0, |c| c.len());
            let parent = world.get::<ChildOf>(e).map(|p| p.parent());
            let at = world
                .get::<bevy::ui::UiGlobalTransform>(e)
                .map(|t| t.translation);
            let _ = write!(
                out,
                " {e} parent={parent:?} at={at:?} size={size:?} display={:?} w={:?} h={:?} bg={} vis={vis:?} kids={children}",
                node.map(|n| n.display),
                node.map(|n| n.width),
                node.map(|n| n.height),
                bg.map_or("-".to_owned(), |c| format!(
                    "{:.2},{:.2},{:.2},{:.2}",
                    c.red, c.green, c.blue, c.alpha
                )),
            );
            if let Some(c) = world.get::<TextColor>(e) {
                let s = c.0.to_srgba();
                let _ = write!(
                    out,
                    " text={:.2},{:.2},{:.2},{:.2}",
                    s.red, s.green, s.blue, s.alpha
                );
            }
            if let Some(f) = world.get::<TextFont>(e) {
                let _ = write!(out, " font={:?}", f.font_size);
            }
        } else {
            let _ = write!(out, " (no entity)");
        }
        for c in n.children.iter().rev() {
            stack.push((*c, depth + 1));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use bevy::input_focus::InputFocus;
    use bevy::prelude::*;
    use mithril::view::{el, IntoView, View};

    use super::{NodeKind, UiTree};
    use crate::{drive_ui, with_host};

    fn form(rows: usize) -> View {
        let mut panel = el("div").class("m-section");
        for i in 0..rows {
            panel = panel.child(
                el("div")
                    .class("m-field")
                    .child(
                        el("label")
                            .class("m-field__label")
                            .child(format!("Field {i}")),
                    )
                    .child(
                        el("input")
                            .class("m-input")
                            .attr("type", "text")
                            .prop("value", "x"),
                    ),
            );
        }
        panel.into_view()
    }

    /// Moving the focus restyles the whole tree (`:focus-within` reaches
    /// every ancestor, and descendants' selectors read it), but writes
    /// components only where the style came out different. Writing them
    /// all made Bevy lay out and shape every text again on every focus
    /// change.
    #[test]
    fn a_focus_change_writes_only_what_changed() {
        let mut world = crate::testing::world();
        with_host(|h| h.mount(form(200)));
        drive_ui(&mut world);
        let nodes = world.resource::<UiTree>().nodes.len();
        assert!(nodes > 600, "{nodes} nodes");
        let input = world
            .resource::<UiTree>()
            .nodes
            .values()
            .find_map(|n| match &n.kind {
                NodeKind::Element { tag, .. } if tag == "input" => n.entity,
                _ => None,
            })
            .expect("an input");
        drive_ui(&mut world);
        world.clear_trackers();
        world
            .resource_mut::<InputFocus>()
            .set(input, bevy::input_focus::FocusCause::Pressed);
        let t = Instant::now();
        drive_ui(&mut world);
        let took = t.elapsed();
        let written = world
            .query::<Ref<Node>>()
            .iter(&world)
            .filter(|n| n.is_changed())
            .count();
        eprintln!("focus change over {nodes} nodes: {took:?}, {written} nodes written");
        assert!(written <= 12, "{written} of {nodes} nodes written");
        // And an idle frame does nothing.
        world.clear_trackers();
        let t = Instant::now();
        drive_ui(&mut world);
        let idle = t.elapsed();
        let written = world
            .query::<Ref<Node>>()
            .iter(&world)
            .filter(|n| n.is_changed())
            .count();
        eprintln!("idle frame: {idle:?}");
        assert_eq!(written, 0);
    }
}
