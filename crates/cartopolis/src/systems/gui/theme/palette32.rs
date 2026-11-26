//! **The egui painters' edge, temporary.** The palette as `egui::Color32`s,
//! for the egui families that remain until each is deleted (egui-removal
//! stage S3, 2026-11-26).
//!
//! The palette itself is [`Palette`], in the theme's own [`Rgba8`]; this is
//! a field-for-field copy of it converted once, at compile time, so the
//! hundreds of egui call sites that hand a palette colour straight to a
//! painter read [`p32`] where they read [`p`] and need no `.into()` each.
//! Nothing new reads it: a screen on the component library reads [`p`]. When
//! the last egui family goes, this file goes with it.

use super::*;

macro_rules! palette32 {
    ($($f:ident),* $(,)?) => {
        /// [`Palette`] as egui's colours — see the module.
        #[derive(Clone, Copy, Debug, PartialEq)]
        pub struct Palette32 {
            $( pub $f: Color32, )*
            pub shadow: f32,
            pub dark: bool,
        }

        impl Palette32 {
            /// Each colour premultiplied as `Color32::from_rgba_unmultiplied`
            /// does, so it is the `Color32` the palette held before S3.
            #[must_use]
            pub const fn of(p: &Palette) -> Self {
                Self {
                    $( $f: p.$f.to_egui(), )*
                    shadow: p.shadow,
                    dark: p.dark,
                }
            }
        }

        /// Every colour field of both, by name.
        #[cfg(test)]
        fn pairs(p: &Palette, q: &Palette32) -> Vec<(&'static str, Rgba8, Color32)> {
            vec![ $( (stringify!($f), p.$f, q.$f), )* ]
        }
    };
}

palette32!(
    ink,
    on_accent,
    on_status,
    surface,
    surface_raised,
    surface_hover,
    line,
    accent,
    accent_bright,
    accent_lift,
    accent_ring,
    accent_pressed,
    accent_text,
    accent_word,
    accent_muted,
    cream,
    text,
    text_dim,
    warn,
    error,
    overlay,
    glass,
    panel,
    card,
    panel_rim,
    overlay_line,
    overlay_soft,
    list_line,
    tile_on,
    knob_on,
    gold,
    on_gold,
    control_edge,
    map_fill,
    map_ink,
    map_rim,
    map_mark,
);

/// [`DARK`] as egui's colours.
pub const DARK32: Palette32 = Palette32::of(&DARK);
/// [`LIGHT`] as egui's colours.
pub const LIGHT32: Palette32 = Palette32::of(&LIGHT);

static DARK32_STATIC: Palette32 = DARK32;
static LIGHT32_STATIC: Palette32 = LIGHT32;

/// [`p`] as egui's colours: the palette in force, for an egui painter.
#[must_use]
pub fn p32() -> &'static Palette32 {
    if p().dark {
        &DARK32_STATIC
    } else {
        &LIGHT32_STATIC
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_field_is_the_palettes_colour() {
        for (p, q) in [(&DARK, &DARK32), (&LIGHT, &LIGHT32)] {
            let pairs = pairs(p, q);
            assert_eq!(pairs.len(), 37);
            for (name, ours, theirs) in pairs {
                assert_eq!(Color32::from(ours), theirs, "{name}");
            }
            assert_eq!((q.dark, q.shadow), (p.dark, p.shadow));
        }
    }
}
