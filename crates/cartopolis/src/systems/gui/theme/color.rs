//! The theme's own colour: sRGB bytes with **straight** alpha.
//!
//! Every colour the theme names was an `egui::Color32` until egui-removal
//! stage S3 (2026-11-26); this is what they are now, so the palette, the
//! tokens and the component library's screens no longer depend on egui.
//!
//! **Straight, not premultiplied.** `Color32` stores its channels multiplied
//! by alpha, so reading a translucent one back (`r()`) does not return the
//! colour it was made from — the trap `tokens::alpha` used to warn about.
//! [`Rgba8`] keeps the bytes it was given; the conversion into `Color32`
//! premultiplies exactly as `Color32::from_rgba_unmultiplied` does, so an
//! egui painter handed `rgba.into()` paints the bytes it painted before.
//!
//! The `Color32` conversions are temporary: they exist for the egui
//! painters that remain until their family is deleted.

/// An sRGB colour, a byte per channel, alpha not premultiplied.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct Rgba8 {
    r: u8,
    g: u8,
    b: u8,
    a: u8,
}

impl Rgba8 {
    pub const WHITE: Self = Self::from_rgb(0xFF, 0xFF, 0xFF);
    pub const BLACK: Self = Self::from_rgb(0, 0, 0);
    /// Fully transparent black — what `Color32::TRANSPARENT` is.
    pub const TRANSPARENT: Self = Self::from_rgba_unmultiplied(0, 0, 0, 0);

    /// An opaque colour.
    #[must_use]
    pub const fn from_rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    /// A colour with straight alpha: `r`, `g`, `b` are the colour itself.
    #[must_use]
    pub const fn from_rgba_unmultiplied(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    #[must_use]
    pub const fn r(self) -> u8 {
        self.r
    }
    #[must_use]
    pub const fn g(self) -> u8 {
        self.g
    }
    #[must_use]
    pub const fn b(self) -> u8 {
        self.b
    }
    #[must_use]
    pub const fn a(self) -> u8 {
        self.a
    }

    /// `[r, g, b, a]`, straight.
    #[must_use]
    pub const fn to_array(self) -> [u8; 4] {
        [self.r, self.g, self.b, self.a]
    }

    /// The same colour at `factor` of its opacity — what callers used
    /// `Color32::gamma_multiply` for. On a premultiplied `Color32` that
    /// multiplies all four bytes, which to the eye leaves the colour and scales
    /// the alpha; here only the alpha moves. The painted bytes can differ from
    /// `gamma_multiply`'s by one step of rounding. Clamped to `0..=1`: a factor
    /// above one brightened a premultiplied colour, which is not an alpha
    /// change and has no meaning here.
    #[must_use]
    pub fn with_alpha_scaled(self, factor: f32) -> Self {
        let f = factor.clamp(0.0, 1.0);
        Self {
            a: (f32::from(self.a) * f + 0.5) as u8,
            ..self
        }
    }

    /// The same colour at alpha `a`.
    #[must_use]
    pub const fn with_alpha(self, a: u8) -> Self {
        Self { a, ..self }
    }

    /// Per channel in sRGB bytes, straight alpha: `t = 0` is `self`, `1` is
    /// `other`. For opaque colours this is `Color32::lerp_to_gamma`.
    #[must_use]
    pub fn lerp(self, other: Self, t: f32) -> Self {
        let t = t.clamp(0.0, 1.0);
        let ch = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * t).round() as u8;
        Self {
            r: ch(self.r, other.r),
            g: ch(self.g, other.g),
            b: ch(self.b, other.b),
            a: ch(self.a, other.a),
        }
    }

    /// As a Bevy colour, in sRGB.
    #[must_use]
    pub fn to_bevy(self) -> bevy::color::Color {
        bevy::color::Color::srgba_u8(self.r, self.g, self.b, self.a)
    }
}

// ─── The egui edge (temporary) ────────────────────────────────────────────────

impl Rgba8 {
    /// As egui's colour, in a `const` (`From` is not): premultiplied as
    /// `Color32::from_rgba_unmultiplied` does, so the bytes are the ones a
    /// `Color32` constant made from the same literal held. For an egui
    /// painter's own `const`; elsewhere `.into()`.
    #[must_use]
    pub const fn to_egui(self) -> egui::Color32 {
        egui::Color32::from_rgba_unmultiplied_const(self.r, self.g, self.b, self.a)
    }
}

impl From<Rgba8> for egui::Color32 {
    /// [`Rgba8::to_egui`].
    fn from(c: Rgba8) -> Self {
        c.to_egui()
    }
}

impl From<Rgba8> for egui::Rgba {
    fn from(c: Rgba8) -> Self {
        egui::Color32::from(c).into()
    }
}

impl From<egui::Color32> for Rgba8 {
    /// Unmultiplies: a translucent `Color32` loses precision in its colour
    /// bytes, which this cannot give back.
    fn from(c: egui::Color32) -> Self {
        let [r, g, b, a] = c.to_srgba_unmultiplied();
        Self { r, g, b, a }
    }
}

#[cfg(test)]
mod tests {
    use super::Rgba8;
    use egui::Color32;

    #[test]
    fn into_color32_is_from_rgba_unmultiplied() {
        for a in [0u8, 1, 22, 90, 140, 150, 235, 254, 255] {
            for (r, g, b) in [(0x14, 0x1A, 0x1C), (0xFF, 0xFF, 0xFF), (0x0F, 0x14, 0x16)] {
                assert_eq!(
                    Color32::from(Rgba8::from_rgba_unmultiplied(r, g, b, a)),
                    Color32::from_rgba_unmultiplied(r, g, b, a)
                );
            }
        }
        assert_eq!(Color32::from(Rgba8::WHITE), Color32::WHITE);
        assert_eq!(Color32::from(Rgba8::BLACK), Color32::BLACK);
        assert_eq!(Color32::from(Rgba8::TRANSPARENT), Color32::TRANSPARENT);
    }

    #[test]
    fn from_color32_unmultiplies() {
        let c = Rgba8::from_rgba_unmultiplied(0xFF, 0xFF, 0xFF, 22);
        assert_eq!(Rgba8::from(Color32::from(c)), c);
        assert_eq!(
            Rgba8::from(Color32::from_rgb(1, 2, 3)),
            Rgba8::from_rgb(1, 2, 3)
        );
    }

    /// `gamma_multiply` on an opaque colour and `with_alpha_scaled` paint the
    /// same, to one step of rounding per byte.
    #[test]
    fn alpha_scaling_matches_gamma_multiply() {
        for f in [0.0, 0.12, 0.35, 0.5, 0.6, 0.85, 1.0] {
            for c in [
                Rgba8::from_rgb(0x2B, 0x83, 0x7E),
                Rgba8::WHITE,
                Rgba8::from_rgb(0xD6, 0x45, 0x45),
            ] {
                let theirs = Color32::from(c).gamma_multiply(f).to_array();
                let ours = Color32::from(c.with_alpha_scaled(f)).to_array();
                for (t, o) in theirs.iter().zip(ours) {
                    assert!(t.abs_diff(o) <= 1, "{c:?} × {f}: {theirs:?} vs {ours:?}");
                }
                assert_eq!(theirs[3], ours[3]);
            }
        }
    }

    #[test]
    fn lerp_of_opaque_colours_is_lerp_to_gamma() {
        let a = Rgba8::from_rgb(0x0F, 0x14, 0x16);
        let b = Rgba8::from_rgb(0xF4, 0xF8, 0xF8);
        for t in [0.0, 0.25, 0.5, 0.9, 1.0] {
            assert_eq!(
                Color32::from(a.lerp(b, t)),
                Color32::from(a).lerp_to_gamma(Color32::from(b), t)
            );
        }
    }

    #[test]
    fn to_bevy_is_srgb() {
        let c = Rgba8::from_rgba_unmultiplied(0x2B, 0x83, 0x7E, 128).to_bevy();
        let s = c.to_srgba();
        assert_eq!(
            [s.red, s.green, s.blue, s.alpha].map(|v| (v * 255.0).round() as u8),
            [0x2B, 0x83, 0x7E, 128]
        );
    }
}
