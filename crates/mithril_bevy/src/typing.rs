//! Typing from keys the app draws itself (an on-screen keyboard, where the
//! platform has no IME): what a key does to the focused text field, as a
//! physical key would. The text goes through the field's `EditableText`
//! (`queue_edit`, applied by `bevy_text`'s `apply_text_edits`, whose change
//! is the field's `input` event); each key is also the field's `keydown`, so
//! an `onkeydown` handler (Enter submits) fires as for a physical key.
//!
//! The keys themselves must not take the focus from the field they type
//! into: an element with the attribute `data-keep-focus` neither takes it on
//! a press nor lets the press clear it (`events.rs`).

use bevy::input_focus::InputFocus;
use bevy::prelude::*;
use bevy::text::{EditableText, TextEdit};

use crate::events::{EventData, UiEvents};
use crate::tree::UiTree;

/// One key on the focused field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyEdit {
    /// Characters, at the cursor (over the selection).
    Text(String),
    Backspace,
    /// A newline in a textarea; in any field, the `keydown` "Enter".
    Enter,
    /// The cursor a character left or right; `select` extends the selection.
    Left {
        select: bool,
    },
    Right {
        select: bool,
    },
    SelectAll,
}

impl KeyEdit {
    /// The `key` its `keydown` carries, as `events.rs` names a physical one.
    fn key(&self) -> String {
        match self {
            KeyEdit::Text(s) => s.clone(),
            KeyEdit::Backspace => "Backspace".into(),
            KeyEdit::Enter => "Enter".into(),
            KeyEdit::Left { .. } => "ArrowLeft".into(),
            KeyEdit::Right { .. } => "ArrowRight".into(),
            KeyEdit::SelectAll => "a".into(),
        }
    }
}

/// Keys waiting for the focused field; `drive_ui` applies them at the start
/// of its frame, so their `keydown`s reach the handlers in that frame.
#[derive(Resource, Default)]
pub struct KeyEdits {
    queue: Vec<KeyEdit>,
}

impl KeyEdits {
    pub fn push(&mut self, edit: KeyEdit) {
        self.queue.push(edit);
    }

    pub(crate) fn take(&mut self) -> Vec<KeyEdit> {
        std::mem::take(&mut self.queue)
    }
}

/// Applies `edit` to the text field that has the focus. False when no text
/// field has it.
pub fn type_into_focused(world: &mut World, edit: KeyEdit) -> bool {
    let Some(entity) = world.get_resource::<InputFocus>().and_then(|f| f.get()) else {
        return false;
    };
    let node = world.resource::<UiTree>().node_of(entity);
    let Some(mut editable) = world.get_mut::<EditableText>(entity) else {
        return false;
    };
    match &edit {
        KeyEdit::Text(s) => editable.queue_edit(TextEdit::Insert(s.as_str().into())),
        KeyEdit::Backspace => editable.queue_edit(TextEdit::Backspace),
        // As `bevy_ui_widgets`' text input takes a physical Enter: a
        // newline only where newlines are allowed.
        KeyEdit::Enter => {
            if editable.allow_newlines {
                editable.queue_edit(TextEdit::Insert("\n".into()));
            }
        }
        KeyEdit::Left { select } => editable.queue_edit(TextEdit::Left(*select)),
        KeyEdit::Right { select } => editable.queue_edit(TextEdit::Right(*select)),
        KeyEdit::SelectAll => editable.queue_edit(TextEdit::SelectAll),
    }
    if let Some(id) = node {
        world.resource_mut::<UiEvents>().push(
            id,
            "keydown",
            EventData {
                key: Some(edit.key()),
                ..Default::default()
            },
        );
    }
    true
}
