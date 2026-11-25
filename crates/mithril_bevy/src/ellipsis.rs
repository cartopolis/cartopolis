//! `text-overflow: ellipsis` for the Bevy backend, which Bevy's text does
//! not have: a one-line text the sheet ends in "…" may shrink to its box
//! (`min-width: 0`), and after the layout it is cut to what fits, with "…".
//!
//! The box asks for the whole text's width (`Node::width`, measured once),
//! not for the text it shows: sized by the cut text, every box around it
//! that sizes to its content would shrink with the cut and keep it cut.
//! When the box's width changes the whole text is put back and cut again,
//! so a line that has room again gets its words back; a cut is a guess
//! from the measured width, and the next frame cuts again if it still does
//! not fit.

use bevy::prelude::*;
use bevy::text::TextLayoutInfo;
use bevy::ui::ComputedNode;

/// A text node that ends in "…" when it does not fit.
#[derive(Component)]
pub(crate) struct Ellipsis {
    /// The whole text.
    full: String,
    /// The whole text's width, logical pixels, once measured.
    pub(crate) full_width: Option<f32>,
    /// The box width it was last fitted to, physical pixels (`-1`: never).
    width: f32,
    /// Frames seen, up to [`SETTLE_FRAMES`]: the first measurements can
    /// come before the faces have loaded, too narrow, so the fit starts
    /// over once when the screen has settled.
    frames: u32,
}

/// How many frames after a text appears its fit starts over, once.
const SETTLE_FRAMES: u32 = 45;

impl Ellipsis {
    pub(crate) fn new(full: String) -> Self {
        Ellipsis {
            full,
            full_width: None,
            width: -1.0,
            frames: 0,
        }
    }

    /// The text changed: measure and fit it from the start.
    pub(crate) fn reset(&mut self, full: String) {
        self.full = full;
        self.full_width = None;
        self.width = -1.0;
    }
}

/// The cut: after `UiSystems::Layout`, when glyphs and boxes are known.
pub(crate) fn fit_ellipsis(
    mut texts: Query<(
        &mut Text,
        &mut Ellipsis,
        &mut Node,
        &ComputedNode,
        &TextLayoutInfo,
    )>,
) {
    for (mut text, mut el, mut node, computed, info) in &mut texts {
        if el.frames < SETTLE_FRAMES {
            el.frames += 1;
            if el.frames == SETTLE_FRAMES {
                let full = el.full.clone();
                el.reset(full);
                el.frames = SETTLE_FRAMES;
            }
        }
        // While the whole text shows, its width is what the box asks for:
        // measured again each time, so a face that loaded late counts.
        if text.0 == el.full && info.size.x > 0.0 {
            let w = info.size.x * computed.inverse_scale_factor;
            if el.full_width.is_none_or(|f| (f - w).abs() > 0.5) {
                el.full_width = Some(w);
                node.width = Val::Px(w);
                continue;
            }
        }
        // First the whole text's width, which the box then asks for.
        let Some(_) = el.full_width else {
            if text.0 != el.full {
                text.0 = el.full.clone();
                node.width = Val::Auto;
                continue;
            }
            if info.size.x > 0.0 {
                let w = info.size.x * computed.inverse_scale_factor;
                el.full_width = Some(w);
                node.width = Val::Px(w);
            }
            continue;
        };
        let width = computed.size.x;
        if width <= 0.0 {
            continue;
        }
        if (width - el.width).abs() > 0.5 {
            // A new width: measure the whole text against it next frame.
            el.width = width;
            if text.0 != el.full {
                text.0 = el.full.clone();
            }
            continue;
        }
        if info.size.x <= width + 1.0 {
            continue;
        }
        let shown = text.0.strip_suffix('…').unwrap_or(&text.0);
        if let Some(cut) = cut_to(shown, width / info.size.x) {
            if text.0 != cut {
                text.0 = cut;
            }
        }
    }
}

/// `text` cut to about `share` of its length, with "…"; at least one
/// character kept.
fn cut_to(text: &str, share: f32) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= 1 {
        return None;
    }
    let keep = ((chars.len() as f32 * share).floor() as usize).clamp(1, chars.len() - 1);
    let head: String = chars[..keep].iter().collect();
    Some(format!("{}…", head.trim_end()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cuts_to_a_share_and_keeps_one_character() {
        assert_eq!(cut_to("Slochterdiep", 0.5).as_deref(), Some("Slocht…"));
        assert_eq!(
            cut_to("Laan Corpus den Hoorn", 0.3).as_deref(),
            Some("Laan C…")
        );
        assert_eq!(cut_to("ab", 0.01).as_deref(), Some("a…"));
        assert_eq!(cut_to("a", 0.5), None);
    }
}
