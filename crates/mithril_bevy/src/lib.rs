//! The component library on Bevy's UI. A `View` is mounted through
//! [`BevyRenderer`], which records what the tree asks for — make a node, set
//! an attribute, insert, listen — as operations; each frame [`drive_ui`]
//! applies them to entities, restyles what changed through the library's
//! cascade ([`mithril::style::Stylist`]), delivers the frame's pointer and
//! keyboard events to the tree's handlers, and applies what those handlers'
//! signal writes queued in turn.
//!
//! Why operations and not direct `World` access: a signal write happens in
//! any system, and the effect it fires has no `World` in hand. Queued, the
//! effect runs anywhere and the entities follow in one place.
//!
//! The host — the renderer, the mounted trees, their handlers — is
//! thread-local, because signals and handlers are: reach it with
//! [`with_host`] from the main thread (an exclusive system, or one that
//! takes [`bevy::ecs::system::NonSendMarker`]).
//!
//! What the renderer draws itself, because the stylesheet draws it with
//! pseudo-elements the cascade does not model: the checkbox tick, the radio
//! dot, the switch knob, the close bars, the select chevron (`tree.rs`,
//! `decorations`).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use bevy::prelude::*;
use mithril::style::{Env, Stylist, Tokens};
use mithril::view::{Handler, Kind, Mounted, Renderer, View};

mod events;
mod popup;
mod tree;

pub use events::{UiEvents, UiPointer};
pub use tree::{FontMap, UiTree};

/// A node as the tree names it: an id the renderer hands out; the entity
/// behind it lives in [`UiTree`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NodeId(pub u64);

/// What the tree asked for, in order.
pub enum Op {
    CreateElement {
        id: NodeId,
        kind: Kind,
        tag: String,
    },
    CreateText {
        id: NodeId,
        text: String,
    },
    CreateAnchor {
        id: NodeId,
    },
    SetText {
        id: NodeId,
        text: String,
    },
    SetAttribute {
        id: NodeId,
        name: String,
        value: Option<String>,
    },
    SetPropertyStr {
        id: NodeId,
        name: String,
        value: Option<String>,
    },
    SetPropertyBool {
        id: NodeId,
        name: String,
        value: bool,
    },
    InsertBefore {
        parent: NodeId,
        node: NodeId,
        before: Option<NodeId>,
    },
    Remove {
        parent: NodeId,
        node: NodeId,
    },
    Listen {
        id: NodeId,
        event: String,
        handler: Handler,
    },
}

/// The `Renderer` the tree talks to: an op queue and an id counter.
#[derive(Default)]
pub struct BevyRenderer {
    pub(crate) ops: Vec<Op>,
    next: u64,
}

impl BevyRenderer {
    fn id(&mut self) -> NodeId {
        self.next += 1;
        NodeId(self.next)
    }

    /// Takes the queued operations.
    pub fn drain(&mut self) -> Vec<Op> {
        std::mem::take(&mut self.ops)
    }
}

impl Renderer for BevyRenderer {
    type Node = NodeId;

    fn create_element(&mut self, kind: Kind, tag: &str) -> NodeId {
        let id = self.id();
        self.ops.push(Op::CreateElement {
            id,
            kind,
            tag: tag.to_owned(),
        });
        id
    }

    fn create_text(&mut self, text: &str) -> NodeId {
        let id = self.id();
        self.ops.push(Op::CreateText {
            id,
            text: text.to_owned(),
        });
        id
    }

    fn create_anchor(&mut self) -> NodeId {
        let id = self.id();
        self.ops.push(Op::CreateAnchor { id });
        id
    }

    fn set_text(&mut self, node: &NodeId, text: &str) {
        self.ops.push(Op::SetText {
            id: *node,
            text: text.to_owned(),
        });
    }

    fn set_attribute(&mut self, node: &NodeId, name: &str, value: Option<&str>) {
        self.ops.push(Op::SetAttribute {
            id: *node,
            name: name.to_owned(),
            value: value.map(str::to_owned),
        });
    }

    fn set_property_str(&mut self, node: &NodeId, name: &str, value: Option<&str>) {
        self.ops.push(Op::SetPropertyStr {
            id: *node,
            name: name.to_owned(),
            value: value.map(str::to_owned),
        });
    }

    fn set_property_bool(&mut self, node: &NodeId, name: &str, value: bool) {
        self.ops.push(Op::SetPropertyBool {
            id: *node,
            name: name.to_owned(),
            value,
        });
    }

    fn insert_before(&mut self, parent: &NodeId, node: &NodeId, before: Option<&NodeId>) {
        self.ops.push(Op::InsertBefore {
            parent: *parent,
            node: *node,
            before: before.copied(),
        });
    }

    fn remove(&mut self, parent: &NodeId, node: &NodeId) {
        self.ops.push(Op::Remove {
            parent: *parent,
            node: *node,
        });
    }

    fn listen(&mut self, node: &NodeId, event: &str, handler: Handler) {
        self.ops.push(Op::Listen {
            id: *node,
            event: event.to_owned(),
            handler,
        });
    }
}

/// The root the tree mounts under: a virtual node the plugin gives an
/// entity when the camera is known.
pub const ROOT: NodeId = NodeId(0);

/// The mounted trees and the renderer they share.
pub struct UiHost {
    pub renderer: Rc<RefCell<BevyRenderer>>,
    mounted: Vec<Mounted<NodeId>>,
    /// Screens mounted under a name, each cleared on its own: a card over
    /// the map beside a panel.
    keyed: HashMap<String, Mounted<NodeId>>,
    /// The renderer's own popup over everything (a select's options).
    popup: Vec<Mounted<NodeId>>,
    /// Handlers by node and event name.
    handlers: HashMap<(NodeId, String), Vec<Handler>>,
    /// Events the renderer raises itself (a popup's choice), delivered in
    /// the same frame after the pending ones.
    pub(crate) internal: Vec<(NodeId, String, events::EventData)>,
    pub stylist: Stylist,
    pub env: Env,
}

impl UiHost {
    pub fn new(tokens: Tokens) -> Self {
        UiHost {
            renderer: Rc::new(RefCell::new(BevyRenderer::default())),
            mounted: Vec::new(),
            keyed: HashMap::new(),
            popup: Vec::new(),
            handlers: HashMap::new(),
            internal: Vec::new(),
            stylist: Stylist::library(tokens),
            env: Env::default(),
        }
    }

    /// Mounts a view under the root; it stays until `clear`.
    pub fn mount(&mut self, view: View) {
        let mounted = mithril::view::mount(view, &self.renderer, &ROOT, None);
        self.mounted.push(mounted);
    }

    /// Unmounts everything.
    pub fn clear(&mut self) {
        self.close_popup();
        let old = std::mem::take(&mut self.mounted);
        for m in old {
            m.unmount(&self.renderer, &ROOT);
        }
        for (_, m) in std::mem::take(&mut self.keyed) {
            m.unmount(&self.renderer, &ROOT);
        }
        self.handlers.clear();
    }

    /// Mounts a screen under `key`, replacing one mounted under it before.
    pub fn mount_keyed(&mut self, key: &str, view: View) {
        self.clear_keyed(key);
        let mounted = mithril::view::mount(view, &self.renderer, &ROOT, None);
        self.keyed.insert(key.to_owned(), mounted);
    }

    /// Unmounts the screen under `key`, leaving the others. Its handlers
    /// go when `drive_ui` sees its nodes removed.
    pub fn clear_keyed(&mut self, key: &str) {
        if let Some(m) = self.keyed.remove(key) {
            self.close_popup();
            m.unmount(&self.renderer, &ROOT);
        }
    }

    /// Mounts the popup's views over everything, after closing any other.
    pub(crate) fn open_popup(&mut self, views: Vec<View>) {
        self.close_popup();
        for view in views {
            let mounted = mithril::view::mount(view, &self.renderer, &ROOT, None);
            self.popup.push(mounted);
        }
    }

    pub(crate) fn close_popup(&mut self) {
        for m in std::mem::take(&mut self.popup) {
            m.unmount(&self.renderer, &ROOT);
        }
    }

    pub(crate) fn handlers_for(&self, id: NodeId, event: &str) -> Vec<Handler> {
        self.handlers
            .get(&(id, event.to_owned()))
            .cloned()
            .unwrap_or_default()
    }
}

thread_local! {
    static HOST: RefCell<Option<UiHost>> = const { RefCell::new(None) };
}

/// The host, on the main thread. Panics off it, or before the plugin ran.
pub fn with_host<R>(f: impl FnOnce(&mut UiHost) -> R) -> R {
    HOST.with(|h| {
        let mut h = h.borrow_mut();
        let host = h
            .as_mut()
            .expect("mithril_bevy: the host is reached on the main thread, after MithrilUiPlugin");
        f(host)
    })
}

/// Which camera the tree draws to, and where its root sits. Insert before
/// the first mount; the root entity is spawned when it appears.
#[derive(Resource, Clone, Copy, Debug)]
pub struct UiCamera(pub Entity);

/// The plugin: the host, the tree, the event queue, the widgets plugins, and
/// [`drive_ui`] in `PostUpdate` (order it with `.before()` / `.after()` on
/// the set [`DriveUi`] as the app needs).
pub struct MithrilUiPlugin {
    pub tokens: Tokens,
}

impl Default for MithrilUiPlugin {
    fn default() -> Self {
        MithrilUiPlugin {
            tokens: Tokens::garage44(),
        }
    }
}

#[derive(SystemSet, Debug, Hash, PartialEq, Eq, Clone)]
pub struct DriveUi;

impl Plugin for MithrilUiPlugin {
    fn build(&self, app: &mut App) {
        if !app.is_plugin_added::<bevy::ui_widgets::TextInputPlugin>() {
            app.add_plugins(bevy::ui_widgets::UiWidgetsPlugins);
        }
        let tokens = self.tokens.clone();
        HOST.with(|h| *h.borrow_mut() = Some(UiHost::new(tokens)));
        app.init_resource::<UiTree>()
            .init_resource::<UiEvents>()
            .init_resource::<UiPointer>()
            .add_systems(PostUpdate, (drive_ui.in_set(DriveUi), spin.after(DriveUi)));
    }
}

/// A node the sheet animates with `m-spin` (the spinner): one turn in 0.7 s.
#[derive(Component)]
pub struct Spin;

fn spin(time: Res<Time>, mut nodes: Query<&mut bevy::ui::UiTransform, With<Spin>>) {
    let turn = (time.elapsed_secs() / 0.7).fract() * std::f32::consts::TAU;
    for mut t in &mut nodes {
        t.rotation = bevy::math::Rot2::radians(turn);
    }
}

/// One frame of the UI: events to handlers, queued operations to entities,
/// changed nodes restyled.
pub fn drive_ui(world: &mut World) {
    // 1. Events → handlers (which write signals → effects → operations). An
    //    event named `@…` is the renderer's own (a select's popup), and a
    //    handler may raise events in turn (`UiHost::internal`): a few
    //    rounds, in the same frame.
    let mut pending = world.resource_mut::<UiEvents>().take();
    let mut rounds = 0;
    while !pending.is_empty() && rounds < 4 {
        rounds += 1;
        let mut calls: Vec<(Handler, events::EventData)> = Vec::new();
        let mut own: Vec<(NodeId, String)> = Vec::new();
        with_host(|host| {
            for (id, event, data) in pending.drain(..) {
                if event.starts_with('@') {
                    own.push((id, event));
                    continue;
                }
                for h in host.handlers_for(id, &event) {
                    calls.push((h, data.clone()));
                }
            }
        });
        for (h, data) in calls {
            h(&data.into_event());
        }
        for (id, event) in own {
            match event.as_str() {
                "@select" => popup::open_select(world, id),
                "@close" => with_host(|host| host.close_popup()),
                _ => {}
            }
        }
        pending = with_host(|host| std::mem::take(&mut host.internal));
    }
    // 2. Operations → the shadow tree and its entities; new listeners.
    let ops = with_host(|host| host.renderer.borrow_mut().drain());
    if !ops.is_empty() {
        if tree::ensure_root(world).is_none() {
            // No camera yet: the whole queue waits, in order.
            with_host(|host| {
                let mut r = host.renderer.borrow_mut();
                let later = std::mem::replace(&mut r.ops, ops);
                r.ops.extend(later);
            });
            return;
        }
        let count = ops.len();
        let removed = ops.iter().any(|op| matches!(op, Op::Remove { .. }));
        let mut listeners = Vec::new();
        for op in ops {
            if let Op::Listen { id, event, handler } = op {
                with_host(|host| {
                    host.handlers
                        .entry((id, event.clone()))
                        .or_default()
                        .push(handler)
                });
                listeners.push((id, event));
            } else {
                tree::apply_op(world, op);
            }
        }
        for (id, event) in listeners {
            events::listen(world, id, &event);
        }
        // A screen cleared on its own leaves its handlers behind (they
        // hold its signals): drop those whose node is gone.
        if removed {
            let tree = world.resource::<UiTree>();
            with_host(|host| {
                host.handlers
                    .retain(|(id, _), _| tree.nodes.contains_key(id))
            });
        }
        let tree = world.resource::<UiTree>();
        debug!(
            ops = count,
            nodes = tree.nodes.len(),
            root = ?tree.root,
            "mithril_bevy: applied"
        );
    }
    // 3. Focus and pointer states, then style.
    tree::sync_states(world);
    let styled = tree::restyle(world);
    if styled > 0 {
        let tree = world.resource::<UiTree>();
        let hidden = tree
            .nodes
            .values()
            .filter(|n| n.computed.as_ref().is_some_and(|c| !c.visible))
            .count();
        let unstyled = tree.nodes.values().filter(|n| n.computed.is_none()).count();
        let root_size = tree
            .root
            .and_then(|r| world.get::<ComputedNode>(r))
            .map(|c| c.size());
        debug!(
            styled,
            hidden,
            unstyled,
            ?root_size,
            "mithril_bevy: restyled"
        );
    }
    if std::env::var_os("MITHRIL_BEVY_DUMP").is_some() {
        // Every two seconds or so, as laid out by then.
        let n = world.resource::<bevy::diagnostic::FrameCount>().0;
        if styled > 0 || n % 120 == 0 {
            debug!("mithril_bevy: tree (frame {n}){}", tree::dump(world, 60));
        }
    }
    tree::sync_inputs(world);
}
