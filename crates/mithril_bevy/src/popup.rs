//! The renderer's own popups: what a browser draws outside the page — a
//! select's options. Built as views and mounted over everything through
//! the host, so they take the library's styles and events like any node.

use bevy::prelude::*;
use bevy::ui::UiGlobalTransform;
use mithril::view::{el, Event, IntoView};

use crate::events::EventData;
use crate::tree::{select_options, NodeKind, UiTree};
use crate::{with_host, NodeId};

/// Opens the options of the select `id` under it (over it when there is no
/// room below). A choice raises the select's `change` with the index; a
/// click outside closes.
pub(crate) fn open_select(world: &mut World, id: NodeId) {
    let (entity, options, selected) = {
        let tree = world.resource::<UiTree>();
        let Some(node) = tree.nodes.get(&id) else {
            return;
        };
        let selected = match &node.kind {
            NodeKind::Element { props, .. } => props
                .get("selectedIndex")
                .and_then(|v| v.parse::<i64>().ok())
                .unwrap_or(-1),
            _ => -1,
        };
        (node.entity, select_options(tree, id), selected)
    };
    let Some(e) = entity else {
        return;
    };
    let (Some(computed), Some(transform)) = (
        world.get::<ComputedNode>(e),
        world.get::<UiGlobalTransform>(e),
    ) else {
        return;
    };
    // Logical pixels, as the tree's styles are.
    let inv = computed.inverse_scale_factor();
    let size = computed.size() * inv;
    let centre = transform.translation * inv;
    let left = centre.x - size.x / 2.0;
    let below = centre.y + size.y / 2.0 + 4.0;
    let estimated = 8.0 + 36.0 * options.len() as f32;
    let viewport_h = with_host(|h| h.env.viewport_height);
    let top = if viewport_h > 0.0 && below + estimated > viewport_h {
        (centre.y - size.y / 2.0 - 4.0 - estimated).max(0.0)
    } else {
        below
    };
    let close = move |_: &Event| {
        with_host(|h| {
            h.internal
                .push((id, "@close".to_owned(), EventData::default()))
        });
    };
    let backdrop = el("button")
        .class("m-select__backdrop")
        .attr("type", "button")
        .attr("aria-label", "Close")
        .on("click", close)
        .into_view();
    let mut menu = el("div").class("m-select__menu").attr(
        "style",
        format!("left: {left}px; top: {top}px; min-width: {}px", size.x),
    );
    for (i, (label, disabled)) in options.into_iter().enumerate() {
        let index = i as i32;
        menu = menu.child(
            el("button")
                .class("m-select__option")
                .attr("type", "button")
                .attr("disabled", disabled)
                .attr(
                    "aria-selected",
                    if i64::from(index) == selected {
                        "true"
                    } else {
                        "false"
                    },
                )
                .on("click", move |_: &Event| {
                    with_host(|h| {
                        h.internal.push((
                            id,
                            "change".to_owned(),
                            EventData {
                                selected_index: Some(index),
                                ..Default::default()
                            },
                        ));
                        h.internal
                            .push((id, "@close".to_owned(), EventData::default()));
                    });
                })
                .child(label),
        );
    }
    with_host(|h| h.open_popup(vec![backdrop, menu.into_view()]));
}
