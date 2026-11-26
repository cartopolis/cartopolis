//! The theme's own colour: sRGB bytes with **straight** alpha.
//!
//! Every colour the theme names was an `egui::Color32` until egui-removal
//! stage S3 (2026-11-26); this is what they are now.
//!
//! **Straight, not premultiplied.** `Color32` stored its channels multiplied
//! by alpha, so reading a translucent one back did not return the colour it
//! was made from — the trap `tokens::alpha` used to warn about. [`Rgba8`]
//! keeps the bytes it was given.

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

#[cfg(test)]
mod tests {
    use super::Rgba8;

    /// Only the alpha moves, rounded to the nearest step, and a factor out of
    /// `0..=1` is clamped.
    #[test]
    fn alpha_scaling_moves_only_the_alpha() {
        let c = Rgba8::from_rgb(0x2B, 0x83, 0x7E);
        for (f, a) in [
            (0.0, 0u8),
            (0.12, 31),
            (0.5, 128),
            (0.85, 217),
            (1.0, 255),
            (2.0, 255),
        ] {
            let s = c.with_alpha_scaled(f);
            assert_eq!(s.to_array(), [0x2B, 0x83, 0x7E, a], "× {f}");
        }
        assert_eq!(Rgba8::WHITE.with_alpha(22).a(), 22);
    }

    /// Per byte in sRGB, ends included.
    #[test]
    fn lerp_runs_per_byte_from_one_end_to_the_other() {
        let a = Rgba8::from_rgb(0x0F, 0x14, 0x16);
        let b = Rgba8::from_rgb(0xF4, 0xF8, 0xF8);
        assert_eq!(a.lerp(b, 0.0), a);
        assert_eq!(a.lerp(b, 1.0), b);
        assert_eq!(a.lerp(b, 0.5).to_array(), [0x82, 0x86, 0x87, 0xFF]);
        assert_eq!(a.lerp(b, -1.0), a, "clamped");
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
