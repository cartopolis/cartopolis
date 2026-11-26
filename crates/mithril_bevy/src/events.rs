//! Pointer, keyboard and text events from Bevy to the tree's handlers. An
//! observer per listened node pushes into [`UiEvents`]; `drive_ui` hands the
//! queue to the handlers at the start of the next frame. Hover and press
//! states for the cascade come from the same observers, on every element
//! that listens.

use bevy::input::keyboard::KeyboardInput;
use bevy::input_focus::{AcquireFocus, FocusLost, FocusedInput, InputFocus};
use bevy::picking::events::{PointerClick, PointerOut, PointerOver, PointerPress, PointerRelease};
use bevy::prelude::*;
use bevy::text::{EditableText, TextEditChange};
use bevy::ui::InteractionDisabled;
use bevy::ui_widgets::ValueChange;
use mithril::view::Event as UiEvent;

use crate::tree::{NodeKind, UiTree};
use crate::NodeId;

/// What an event carries, in a form a resource may hold (`UiEvent` itself
/// has a `Cell`).
#[derive(Clone, Debug, Default)]
pub struct EventData {
    pub value: Option<String>,
    pub checked: Option<bool>,
    pub selected_index: Option<i32>,
    pub key: Option<String>,
}

impl EventData {
    pub(crate) fn into_event(self) -> UiEvent {
        let mut event = UiEvent::default();
        event.value = self.value;
        event.checked = self.checked;
        event.selected_index = self.selected_index;
        event.key = self.key;
        event
    }
}

/// Events waiting for the tree's handlers: node, event name, data.
#[derive(Resource, Default)]
pub struct UiEvents {
    queue: Vec<(NodeId, String, EventData)>,
}

impl UiEvents {
    pub fn push(&mut self, id: NodeId, event: &str, data: EventData) {
        self.queue.push((id, event.to_owned(), data));
    }

    pub(crate) fn take(&mut self) -> Vec<(NodeId, String, EventData)> {
        std::mem::take(&mut self.queue)
    }
}

/// Whether the pointer is over any node of the tree: what a world click
/// yields to.
#[derive(Resource, Default)]
pub struct UiPointer {
    pub over: std::collections::HashSet<Entity>,
}

impl UiPointer {
    pub fn owns_pointer(&self) -> bool {
        !self.over.is_empty()
    }
}

fn text_of(texts: &Query<&EditableText>, entity: Entity) -> EventData {
    let mut data = EventData::default();
    if let Ok(editable) = texts.get(entity) {
        data.value = Some(editable.editor.text().to_string());
    }
    data
}

/// Attaches the observers a listened event needs. Hover and press
/// observers are added once per node, by the first listener.
pub(crate) fn listen(world: &mut World, id: NodeId, event: &str) {
    let (entity, tag, input_type) = {
        let tree = world.resource::<UiTree>();
        let Some(node) = tree.nodes.get(&id) else {
            return;
        };
        let Some(entity) = node.entity else {
            return;
        };
        match &node.kind {
            NodeKind::Element { tag, attrs, .. } => {
                let input_type = attrs
                    .iter()
                    .find(|(n, _)| n == "type")
                    .map(|(_, v)| v.clone())
                    .unwrap_or_default();
                (entity, tag.clone(), input_type)
            }
            _ => return,
        }
    };
    ensure_state_observers(world, entity, id);
    let checkbox_like = tag == "input" && matches!(input_type.as_str(), "checkbox" | "radio");
    match event {
        "click" => {
            world.entity_mut(entity).observe(
                move |mut ev: On<PointerClick>,
                      disabled: Query<&InteractionDisabled>,
                      mut events: ResMut<UiEvents>| {
                    if disabled.get(ev.entity).is_ok() {
                        return;
                    }
                    ev.propagate(false);
                    events.push(id, "click", EventData::default());
                },
            );
        }
        "change" if checkbox_like => {
            // A click toggles the drawn control; the handler gets the new
            // state and writes the model, which draws it.
            let radio = input_type == "radio";
            world.entity_mut(entity).observe(
                move |mut ev: On<PointerClick>, tree: Res<UiTree>, mut events: ResMut<UiEvents>| {
                    ev.propagate(false);
                    let checked = tree.nodes.get(&id).is_some_and(|n| match &n.kind {
                        NodeKind::Element { states, .. } => states.checked,
                        _ => false,
                    });
                    let data = EventData {
                        checked: Some(if radio { true } else { !checked }),
                        ..Default::default()
                    };
                    events.push(id, "change", data);
                },
            );
        }
        "input" if tag == "input" && input_type == "range" => {
            // A range: the headless slider's value, while and after a drag.
            world.entity_mut(entity).observe(
                move |ev: On<ValueChange<f32>>, mut events: ResMut<UiEvents>| {
                    events.push(
                        id,
                        "input",
                        EventData {
                            value: Some(format!("{}", ev.value)),
                            ..Default::default()
                        },
                    );
                },
            );
        }
        "input" => {
            // Text: every applied edit is an input.
            world.entity_mut(entity).observe(
                move |ev: On<TextEditChange>,
                      texts: Query<&EditableText>,
                      mut events: ResMut<UiEvents>| {
                    let data = text_of(&texts, ev.event_target());
                    events.push(id, "input", data);
                },
            );
        }
        "change" if tag == "select" => {
            // A select: a click opens its popup (`popup::open_select`), whose
            // choice comes back as the change.
            world.entity_mut(entity).observe(
                move |mut ev: On<PointerClick>,
                      disabled: Query<&InteractionDisabled>,
                      mut events: ResMut<UiEvents>| {
                    if disabled.get(ev.entity).is_ok() {
                        return;
                    }
                    ev.propagate(false);
                    events.push(id, "@select", EventData::default());
                },
            );
        }
        "change" => {
            // Text: a change when focus leaves.
            world.entity_mut(entity).observe(
                move |ev: On<FocusLost>,
                      texts: Query<&EditableText>,
                      mut events: ResMut<UiEvents>| {
                    let data = text_of(&texts, ev.event_target());
                    events.push(id, "change", data);
                },
            );
        }
        "blur" => {
            world.entity_mut(entity).observe(
                move |_ev: On<FocusLost>, mut events: ResMut<UiEvents>| {
                    events.push(id, "blur", EventData::default());
                },
            );
        }
        "keydown" => {
            world.entity_mut(entity).observe(
                move |ev: On<FocusedInput<KeyboardInput>>, mut events: ResMut<UiEvents>| {
                    let input = &ev.event().input;
                    if input.state != bevy::input::ButtonState::Pressed {
                        return;
                    }
                    use bevy::input::keyboard::Key;
                    let key = match &input.logical_key {
                        Key::Character(c) => c.to_string(),
                        Key::Enter => "Enter".into(),
                        Key::Escape => "Escape".into(),
                        Key::Backspace => "Backspace".into(),
                        Key::ArrowUp => "ArrowUp".into(),
                        Key::ArrowDown => "ArrowDown".into(),
                        Key::ArrowLeft => "ArrowLeft".into(),
                        Key::ArrowRight => "ArrowRight".into(),
                        Key::Home => "Home".into(),
                        Key::End => "End".into(),
                        Key::Tab => "Tab".into(),
                        other => format!("{other:?}"),
                    };
                    events.push(
                        id,
                        "keydown",
                        EventData {
                            key: Some(key),
                            ..Default::default()
                        },
                    );
                },
            );
        }
        _ => {}
    }
}

#[derive(Component)]
struct StateObserved;

/// Hover and press, for `:hover`/`:active`; focus on press for controls.
fn ensure_state_observers(world: &mut World, entity: Entity, id: NodeId) {
    if world.get::<StateObserved>(entity).is_some() {
        return;
    }
    world.entity_mut(entity).insert(StateObserved);
    world.entity_mut(entity).observe(
        move |ev: On<PointerOver>, mut tree: ResMut<UiTree>, mut pointer: ResMut<UiPointer>| {
            pointer.over.insert(ev.entity);
            set_state(&mut tree, id, |s| s.hover = true);
        },
    );
    world.entity_mut(entity).observe(
        move |ev: On<PointerOut>, mut tree: ResMut<UiTree>, mut pointer: ResMut<UiPointer>| {
            pointer.over.remove(&ev.entity);
            set_state(&mut tree, id, |s| {
                s.hover = false;
                s.active = false;
            });
        },
    );
    world.entity_mut(entity).observe(
        move |ev: On<PointerPress>,
              mut tree: ResMut<UiTree>,
              mut focus: ResMut<InputFocus>,
              inputs: Query<&bevy::ui_widgets::TextInput>| {
            set_state(&mut tree, id, |s| s.active = true);
            // A press on a control takes the focus; a text input does this
            // itself; an on-screen keyboard's key leaves it where it is.
            if inputs.get(ev.entity).is_err() && !keeps_focus(&tree, id) {
                focus.set(ev.entity, bevy::input_focus::FocusCause::Pressed);
            }
        },
    );
    // The press's own request for focus (`PointerFocusPlugin`) bubbles to
    // the window, which clears the focus ("click outside to unfocus"): a
    // key that keeps the focus stops it on its way up.
    world
        .entity_mut(entity)
        .observe(move |mut ev: On<AcquireFocus>, tree: Res<UiTree>| {
            if keeps_focus(&tree, id) {
                ev.propagate(false);
            }
        });
    world
        .entity_mut(entity)
        .observe(move |_ev: On<PointerRelease>, mut tree: ResMut<UiTree>| {
            set_state(&mut tree, id, |s| s.active = false);
        });
}

/// An element marked `data-keep-focus` (an on-screen keyboard and its keys,
/// `typing.rs`): pressing it leaves the focus on the field being typed in.
fn keeps_focus(tree: &UiTree, id: NodeId) -> bool {
    tree.nodes.get(&id).is_some_and(|n| match &n.kind {
        NodeKind::Element { attrs, .. } => attrs.iter().any(|(name, _)| name == "data-keep-focus"),
        _ => false,
    })
}

fn set_state(tree: &mut UiTree, id: NodeId, f: impl FnOnce(&mut mithril::style::States)) {
    if let Some(n) = tree.nodes.get_mut(&id) {
        if let NodeKind::Element { states, .. } = &mut n.kind {
            f(states);
            n.restyle = true;
        }
    }
    // Descendants read `:hover` through selectors like `&:hover .x`.
    let children: Vec<NodeId> = tree
        .nodes
        .get(&id)
        .map(|n| n.children.clone())
        .unwrap_or_default();
    for c in children {
        tree.mark_restyle(c);
    }
}
