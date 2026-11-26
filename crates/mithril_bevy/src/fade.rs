//! The sheet's colour transitions: a node's background and border ease to
//! a new colour over its `transition`'s duration, instead of jumping in one
//! frame. The press itself does not ease: the sheet's `:active` rules set
//! `transition-duration: 0ms`, and a transition takes the duration of the
//! state it goes *to*, as in the browser. So a tap shows at once and its
//! release fades back.
//!
//! The restyle (`tree.rs`) writes the target colours as before; this runs
//! after it, notices the write through change detection, and puts the
//! colour shown so far back for the frames the fade lasts. Its own writes
//! bypass change detection, so the next change it sees is the restyle's.

use bevy::prelude::*;

use crate::tree::UiTree;

/// A node's colours on their way from `from` to `to`.
#[derive(Component)]
pub(crate) struct Fade {
    from: Colours,
    to: Colours,
    start: f32,
    secs: f32,
}

#[derive(Clone, Copy, PartialEq)]
struct Colours {
    bg: Color,
    border: [Color; 4],
}

impl Colours {
    fn of(bg: &BackgroundColor, border: Option<&BorderColor>) -> Self {
        let border = border.map_or([Color::NONE; 4], |b| [b.top, b.right, b.bottom, b.left]);
        Colours { bg: bg.0, border }
    }

    fn mix(&self, other: &Colours, t: f32) -> Colours {
        let m = |a: Color, b: Color| a.mix(&b, t);
        Colours {
            bg: m(self.bg, other.bg),
            border: [
                m(self.border[0], other.border[0]),
                m(self.border[1], other.border[1]),
                m(self.border[2], other.border[2]),
                m(self.border[3], other.border[3]),
            ],
        }
    }
}

impl Fade {
    fn shown(&self, now: f32) -> Colours {
        if self.secs <= 0.0 {
            return self.to;
        }
        let t = ((now - self.start) / self.secs).clamp(0.0, 1.0);
        self.from.mix(&self.to, ease(t))
    }

    fn running(&self, now: f32) -> bool {
        self.secs > 0.0 && now < self.start + self.secs
    }
}

/// Close to the sheet's `cubic-bezier(0.2, 0, 0, 1)`: fast out, slow in.
fn ease(t: f32) -> f32 {
    1.0 - (1.0 - t).powi(3)
}

/// The seconds a node's colour change takes, from its computed
/// `transition-duration` or `transition`: the first time in it, or none.
fn duration(tree: &UiTree, entity: Entity) -> f32 {
    let Some(c) = tree
        .node_of(entity)
        .and_then(|id| tree.nodes.get(&id))
        .and_then(|n| n.computed.as_ref())
    else {
        return 0.0;
    };
    let value = c
        .other
        .get("transition-duration")
        .or_else(|| c.other.get("transition"));
    value.map_or(0.0, |v| {
        // Only a transition that names a colour (or `all`) fades one.
        if c.other.get("transition-duration").is_none()
            && !["background", "border", "all"]
                .iter()
                .any(|p| v.contains(p))
        {
            return 0.0;
        }
        first_time(v)
    })
}

/// The first `<n>ms` or `<n>s` in a value.
fn first_time(v: &str) -> f32 {
    for word in v.split(|ch: char| ch.is_whitespace() || ch == ',') {
        if let Some(ms) = word.strip_suffix("ms").and_then(|n| n.parse::<f32>().ok()) {
            return ms / 1000.0;
        }
        if let Some(s) = word.strip_suffix('s').and_then(|n| n.parse::<f32>().ok()) {
            return s;
        }
    }
    // An unresolved `var(--m-duration)`: the library's own.
    if v.contains("var(--m-duration)") {
        return 0.14;
    }
    0.0
}

pub(crate) fn fade(
    mut commands: Commands,
    time: Res<Time>,
    tree: Res<UiTree>,
    mut nodes: Query<
        (
            Entity,
            &mut BackgroundColor,
            Option<&mut BorderColor>,
            Option<&mut Fade>,
        ),
        Or<(Changed<BackgroundColor>, Changed<BorderColor>, With<Fade>)>,
    >,
) {
    let now = time.elapsed_secs();
    for (entity, mut bg, mut border, fade) in &mut nodes {
        let written = Colours::of(&bg, border.as_deref());
        let changed = bg.is_changed() || border.as_ref().is_some_and(|b| b.is_changed());
        let Some(mut fade) = fade else {
            // First sight: nothing to fade from. Only the tree's nodes.
            if tree.node_of(entity).is_some() {
                commands.entity(entity).try_insert(Fade {
                    from: written,
                    to: written,
                    start: now,
                    secs: 0.0,
                });
            }
            continue;
        };
        if changed && written != fade.to {
            let shown = fade.shown(now);
            let secs = duration(&tree, entity);
            *fade = Fade {
                from: shown,
                to: written,
                start: now,
                secs,
            };
        }
        if !fade.running(now) {
            // Done, or never started: the restyle's colour stands. A
            // restyle that rewrote the same colour mid-fade left it there.
            if fade.secs > 0.0 {
                fade.secs = 0.0;
                fade.from = fade.to;
            }
            continue;
        }
        let shown = fade.shown(now);
        bg.bypass_change_detection().0 = shown.bg;
        if let Some(b) = border.as_mut() {
            let b = b.bypass_change_detection();
            [b.top, b.right, b.bottom, b.left] = shown.border;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_transition_says_its_time_in_ms_or_s() {
        assert_eq!(
            first_time("background 140ms cubic-bezier(0.2, 0, 0, 1)"),
            0.14
        );
        assert_eq!(first_time("0ms"), 0.0);
        assert_eq!(first_time("color 0.3s ease, background 1s"), 0.3);
        assert_eq!(
            first_time("background var(--m-duration) var(--m-ease)"),
            0.14
        );
        assert_eq!(first_time("none"), 0.0);
    }

    #[test]
    fn a_fade_eases_from_where_it_was_to_where_it_goes() {
        let c = |v: f32| Colours {
            bg: Color::srgb(v, v, v),
            border: [Color::NONE; 4],
        };
        let f = Fade {
            from: c(0.0),
            to: c(1.0),
            start: 10.0,
            secs: 0.2,
        };
        assert!(f.running(10.1));
        assert!(!f.running(10.3));
        assert_eq!(f.shown(10.0).bg, c(0.0).bg);
        assert_eq!(f.shown(11.0).bg, c(1.0).bg);
        let mid = f.shown(10.1).bg.to_srgba().red;
        assert!(
            mid > 0.5 && mid < 1.0,
            "fast out: past half way at half time, {mid}"
        );
    }
}
