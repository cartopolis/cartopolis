//! A tree with no app around it, for tests here and in the client: a
//! `World` with the resources [`crate::drive_ui`] reads and the host on this
//! thread, and a stand-in for `bevy_ui`'s layout, which needs a camera with
//! a render target that a test does not have.

use bevy::prelude::*;
use bevy::ui::{ComputedNode, UiGlobalTransform, UiStack};
use mithril::style::Tokens;

use crate::events::{UiEvents, UiPointer};
use crate::tree::{NodeKind, UiTree};
use crate::{images, KeyEdits, LiveImages, NodeId, UiCamera, UiHost, HOST, ROOT};

/// A world the tree can be mounted in and driven (`with_host(|h|
/// h.mount(view))`, then [`crate::drive_ui`]), with the host on this
/// thread replaced by a fresh one.
pub fn world() -> World {
    let mut world = World::new();
    world.init_resource::<UiTree>();
    world.init_resource::<images::ImageCache>();
    world.init_resource::<LiveImages>();
    world.init_resource::<UiEvents>();
    world.init_resource::<UiPointer>();
    world.init_resource::<KeyEdits>();
    world.init_resource::<bevy::input_focus::InputFocus>();
    world.init_resource::<Assets<Image>>();
    let camera = world.spawn_empty().id();
    world.insert_resource(UiCamera(camera));
    HOST.with(|h| *h.borrow_mut() = Some(UiHost::new(Tokens::garage44())));
    world
}

/// Height of a row of [`lay_out_rows`], physical px.
pub const ROW: f32 = 40.0;

/// Lays every node out as `bevy_ui` would have, in a simpler shape: each
/// node of the tree its own row, `ROW` high and 200 wide (physical px), in
/// tree order, stacked in the same order, at `scale` physical px per
/// logical px. Hidden nodes keep their rows; the walk must leave them out
/// by their style. Returns the nodes in row order.
pub fn lay_out_rows(world: &mut World, scale: f32) -> Vec<NodeId> {
    let (root, order): (Entity, Vec<(NodeId, Entity)>) = {
        let tree = world.resource::<UiTree>();
        let Some(root) = tree.root else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut stack = vec![ROOT];
        while let Some(id) = stack.pop() {
            let Some(n) = tree.nodes.get(&id) else {
                continue;
            };
            if id != ROOT {
                if let Some(e) = n.entity {
                    if !matches!(n.kind, NodeKind::Anchor) {
                        out.push((id, e));
                    }
                }
            }
            stack.extend(n.children.iter().rev().copied());
        }
        (root, out)
    };
    let inverse = 1.0 / scale;
    let height = ROW * (order.len() as f32 + 1.0);
    world.entity_mut(root).insert((
        ComputedNode {
            size: Vec2::new(400.0, height),
            inverse_scale_factor: inverse,
            ..default()
        },
        UiGlobalTransform::from_translation(Vec2::new(200.0, height / 2.0)),
    ));
    let mut uinodes = vec![root];
    for (i, (_, e)) in order.iter().enumerate() {
        world.entity_mut(*e).insert((
            ComputedNode {
                size: Vec2::new(200.0, ROW - 8.0),
                inverse_scale_factor: inverse,
                ..default()
            },
            UiGlobalTransform::from_translation(Vec2::new(110.0, ROW * i as f32 + ROW / 2.0)),
        ));
        uinodes.push(*e);
    }
    // What `bevy_camera`'s visibility propagation would work out; without
    // it every `InheritedVisibility` stays at its default, hidden.
    for &e in &uinodes {
        let mut shown = true;
        let mut cur = Some(e);
        while let Some(c) = cur {
            shown &= world.get::<Visibility>(c) != Some(&Visibility::Hidden);
            cur = world.get::<ChildOf>(c).map(ChildOf::parent);
        }
        let v = if shown {
            InheritedVisibility::VISIBLE
        } else {
            InheritedVisibility::HIDDEN
        };
        world.entity_mut(e).insert(v);
    }
    let len = uinodes.len();
    world.insert_resource(UiStack {
        partition: vec![0..len],
        uinodes,
    });
    order.into_iter().map(|(id, _)| id).collect()
}
