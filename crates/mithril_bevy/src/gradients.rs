//! CSS gradients as `bevy_ui`'s `BackgroundGradient`: what a lit planet or
//! a fading veil needs, not the whole of CSS. The cascade keeps a
//! gradient's text as it was written (`background: linear-gradient(…)`, or
//! `background-image: …`, which it does not know), its `var()`s already
//! filled in.
//!
//! - `linear-gradient([<angle>deg | to top|bottom|left|right,] stops)`
//! - `radial-gradient([circle] [closest-side | farthest-side |
//!   farthest-corner] [at <x>% <y>%], stops)`
//!
//! A stop is a colour and, if any, one `%` or `px` position. Anything else
//! is not drawn, and says so once in the log.

use bevy::prelude::*;
use mithril::style::cascade::Rgba;

/// The gradients `value` names, or `None` when it is not one this reads.
pub(crate) fn parse(value: &str, opacity: f32) -> Option<Vec<Gradient>> {
    let value = value.trim();
    if let Some(inner) = function(value, "linear-gradient") {
        return linear(inner, opacity).map(|g| vec![g]);
    }
    if let Some(inner) = function(value, "radial-gradient") {
        return radial(inner, opacity).map(|g| vec![g]);
    }
    None
}

fn function<'a>(value: &'a str, name: &str) -> Option<&'a str> {
    value
        .strip_prefix(name)?
        .trim_start()
        .strip_prefix('(')?
        .strip_suffix(')')
}

/// Splits on commas outside parentheses, so `rgb(0 0 0 / 40%)` stays whole.
fn args(inner: &str) -> Vec<&str> {
    let (mut out, mut depth, mut start) = (Vec::new(), 0usize, 0usize);
    for (i, ch) in inner.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                out.push(inner[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(inner[start..].trim());
    out
}

fn linear(inner: &str, opacity: f32) -> Option<Gradient> {
    let parts = args(inner);
    let (angle, from) = match parts.first().map(|p| direction(p)) {
        Some(Some(a)) => (a, 1),
        _ => (LinearGradient::TO_BOTTOM, 0),
    };
    let stops = stops(&parts[from..], opacity)?;
    Some(Gradient::Linear(LinearGradient {
        angle,
        stops,
        ..default()
    }))
}

/// A linear gradient's direction in Bevy's radians (0 up, clockwise), as
/// CSS's degrees are.
fn direction(p: &str) -> Option<f32> {
    if let Some(deg) = p.strip_suffix("deg") {
        return deg.trim().parse::<f32>().ok().map(f32::to_radians);
    }
    Some(match p {
        "to top" => LinearGradient::TO_TOP,
        "to right" => LinearGradient::TO_RIGHT,
        "to bottom" => LinearGradient::TO_BOTTOM,
        "to left" => LinearGradient::TO_LEFT,
        _ => return None,
    })
}

fn radial(inner: &str, opacity: f32) -> Option<Gradient> {
    let parts = args(inner);
    let first = parts.first()?;
    let (shape, position, from) = if Rgba::parse(first.split_whitespace().next()?).is_some()
        || first.starts_with("rgb")
        || first.starts_with('#')
    {
        (RadialGradientShape::FarthestCorner, UiPosition::CENTER, 0)
    } else {
        let (shape_part, at) = match first.split_once("at ") {
            Some((s, at)) => (s.trim(), Some(at.trim())),
            None => (first.trim(), None),
        };
        let circle = shape_part.contains("circle");
        let shape = if shape_part.contains("closest-side") {
            RadialGradientShape::ClosestSide
        } else if shape_part.contains("farthest-side") {
            RadialGradientShape::FarthestSide
        } else if circle {
            // CSS's circle reaches the farthest corner; the nearest Bevy
            // has for a circle is its farthest side.
            RadialGradientShape::FarthestSide
        } else {
            RadialGradientShape::FarthestCorner
        };
        let position = match at {
            Some(at) => {
                let mut xy = at.split_whitespace().filter_map(percent);
                let x = xy.next()?;
                let y = xy.next().unwrap_or(0.5);
                UiPosition::anchor(Vec2::new(x - 0.5, y - 0.5))
            }
            None => UiPosition::CENTER,
        };
        (shape, position, 1)
    };
    let stops = stops(&parts[from..], opacity)?;
    Some(Gradient::Radial(RadialGradient {
        position,
        shape,
        stops,
        ..default()
    }))
}

fn percent(p: &str) -> Option<f32> {
    match p {
        "center" => Some(0.5),
        "left" | "top" => Some(0.0),
        "right" | "bottom" => Some(1.0),
        _ => p
            .strip_suffix('%')?
            .trim()
            .parse::<f32>()
            .ok()
            .map(|v| v / 100.0),
    }
}

fn stops(parts: &[&str], opacity: f32) -> Option<Vec<ColorStop>> {
    if parts.len() < 2 {
        return None;
    }
    parts
        .iter()
        .map(|p| {
            // The colour is everything up to the last word, when that word
            // is a position: `rgb(0 0 0 / 40%) 60%` keeps its own `40%`.
            let (colour, at) = match p.rsplit_once(' ') {
                Some((c, at)) if !c.ends_with('/') && position(at).is_some() => {
                    (c.trim(), position(at))
                }
                _ => (p.trim(), None),
            };
            let c = Rgba::parse(colour)?;
            let colour = Color::srgba(c.r, c.g, c.b, c.a * opacity);
            Some(match at {
                Some(val) => ColorStop::new(colour, val),
                None => ColorStop::auto(colour),
            })
        })
        .collect()
}

fn position(p: &str) -> Option<Val> {
    if let Some(v) = p.strip_suffix('%') {
        return v.parse::<f32>().ok().map(Val::Percent);
    }
    p.strip_suffix("px")?.parse::<f32>().ok().map(Val::Px)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_lit_sphere_and_a_veil() {
        let lit = parse(
            "radial-gradient(circle at 35% 30%, #ffffff, rgb(20 40 80 / 80%) 70%, #000000)",
            1.0,
        )
        .expect("a radial gradient");
        match &lit[0] {
            Gradient::Radial(r) => {
                assert_eq!(r.stops.len(), 3);
                assert!((r.position.anchor.x + 0.15).abs() < 1e-4);
                assert!((r.position.anchor.y + 0.2).abs() < 1e-4);
            }
            other => panic!("not radial: {other:?}"),
        }
        let veil = parse(
            "linear-gradient(to top, rgb(0 0 0 / 60%), transparent)",
            1.0,
        )
        .expect("a linear gradient");
        match &veil[0] {
            Gradient::Linear(l) => {
                assert_eq!(l.angle, LinearGradient::TO_TOP);
                assert_eq!(l.stops.len(), 2);
            }
            other => panic!("not linear: {other:?}"),
        }
    }

    #[test]
    fn refuses_what_it_cannot_draw() {
        assert!(parse("conic-gradient(red, blue)", 1.0).is_none());
        assert!(parse("linear-gradient(red)", 1.0).is_none());
        assert!(parse("#ff0000", 1.0).is_none());
    }
}
