//! The checks on a script's rule tables — `particles{…}`, `set_tone{…}` — and on
//! sound names, written once for both engines.
//!
//! A Luau script hands over a table, a JavaScript one an object that reaches the
//! simulator as JSON (`bun.rs`); [`Rules`] is what the two have in common, so
//! that "which keys are allowed, what a missing one means, what is clamped" is
//! one function per call rather than one per engine to keep in step.

use cartopolis_core::tone::{note_hz, Voice};
use cartopolis_core::{ParticleEffect, PrimSound, SoundName, ToneSpec, DEFAULT_SOUND_RANGE_M};

/// A pitch as written: hertz, a note name, or something else.
pub(crate) enum Pitch {
    Hz(f32),
    Note(String),
    Missing,
    Other,
}

/// A table of named rules, as either engine hands it over.
pub(crate) trait Rules {
    /// Every key present, as text (a non-text key reads as an empty name).
    fn keys(&self) -> Result<Vec<String>, String>;
    fn num(&self, key: &str) -> Result<Option<f32>, String>;
    /// A colour written positionally, `{1.0, 0.5, 0.1}` / `[1.0, 0.5, 0.1]`.
    fn rgb(&self, key: &str) -> Result<Option<[Option<f32>; 3]>, String>;
    fn text(&self, key: &str) -> Result<Option<String>, String>;
    fn pitch(&self, key: &str) -> Result<Pitch, String>;
}

pub(crate) fn finite(v: f32) -> Result<f32, String> {
    if v.is_finite() {
        Ok(v)
    } else {
        Err("expected finite numbers".to_string())
    }
}

/// Refuse a key not in `keys`. A tone whose `vibratto = 0.5` quietly did
/// nothing would be found by ear, which is the slowest way to find a typo.
fn only_keys(t: &dyn Rules, what: &str, keys: &[&str]) -> Result<(), String> {
    for name in t.keys()? {
        if !keys.contains(&name.as_str()) {
            return Err(format!(
                "{what} `{name}` is not one of: {}",
                keys.join(", ")
            ));
        }
    }
    Ok(())
}

/// Read a `particles` rules table into an effect.
///
/// Unspecified fields keep the preset's value, which is what makes
/// `particles{ color_start = {0,0.4,1} }` a blue flame rather than an error
/// message about the eleven fields it did not mention. An **unknown** key is an
/// error, though, and deliberately so: `particles{ lifetime = 3 }` silently
/// emitting the default 1.4 s is the kind of bug an author cannot see, and this
/// is the same rule the shot manifest applies to its own keys.
///
/// The names are **exactly** `authoring::ParticleSpec`'s, and
/// `the_two_authoring_surfaces_use_one_vocabulary` is the test that keeps them
/// so. They were shorter here once — `spread`, `radius`, `size`, `color` — which
/// meant the same emitter was spelled two ways depending on whether a script or
/// an HTTP body set it, and a person moving between them had to translate. The
/// longer names won because they are symmetric (`size_start` beside `size_end`,
/// where `size` beside `end_size` is not) and because `spread_deg` carries its
/// unit, which this codebase has reason to care about.
pub(crate) fn effect(t: &dyn Rules, burst: bool, sequence: u32) -> Result<ParticleEffect, String> {
    const KEYS: &[&str] = &[
        "count",
        "life",
        "speed",
        "spread_deg",
        "gravity",
        "source_radius",
        "size_start",
        "size_end",
        "color_start",
        "color_end",
        "alpha",
        "turbulence",
    ];
    for name in t.keys()? {
        if !KEYS.contains(&name.as_str()) {
            // An old spelling gets its replacement by name rather than the whole
            // list. These six were the script API's own names until the two
            // authoring surfaces were reconciled, so a script written against
            // them is somebody's real work and deserves better than "not one of:
            // twelve things".
            let hint = match name.as_str() {
                "spread" => Some("spread_deg"),
                "radius" => Some("source_radius"),
                "size" => Some("size_start"),
                "end_size" => Some("size_end"),
                "color" => Some("color_start"),
                "end_color" => Some("color_end"),
                _ => None,
            };
            return Err(match hint {
                Some(now) => format!("particle rule `{name}` is called `{now}` now"),
                None => format!("particle rule `{name}` is not one of: {}", KEYS.join(", ")),
            });
        }
    }

    let base = if burst {
        ParticleEffect::blast()
    } else {
        ParticleEffect::fire()
    };
    let num = |key: &str, fallback: f32| -> Result<f32, String> {
        match t.num(key)? {
            Some(v) => finite(v),
            None => Ok(fallback),
        }
    };
    let rgb = |key: &str, fallback: [f32; 3]| -> Result<[f32; 3], String> {
        let Some(c) = t.rgb(key)? else {
            return Ok(fallback);
        };
        Ok([
            finite(c[0].unwrap_or(fallback[0]))?,
            finite(c[1].unwrap_or(fallback[1]))?,
            finite(c[2].unwrap_or(fallback[2]))?,
        ])
    };

    Ok(ParticleEffect {
        count: num("count", f32::from(base.count))?.clamp(0.0, 65_535.0) as u16,
        life: num("life", base.life)?,
        speed: num("speed", base.speed)?,
        spread_deg: num("spread_deg", base.spread_deg)?,
        gravity: num("gravity", base.gravity)?,
        source_radius: num("source_radius", base.source_radius)?,
        size_start: num("size_start", base.size_start)?,
        size_end: num("size_end", base.size_end)?,
        color_start: rgb("color_start", base.color_start)?,
        color_end: rgb("color_end", base.color_end)?,
        alpha: num("alpha", base.alpha)?,
        turbulence: num("turbulence", base.turbulence)?,
        burst,
        sequence,
    }
    // Clamped here, at the boundary, exactly as `set_scale` clamps rather than
    // refuses: a script that asks for a thousand particles gets the ceiling and
    // keeps running.
    .sanitized())
}

/// Read `play_sound` / `loop_sound`'s arguments into a [`PrimSound`].
///
/// An unknown name is an **error** naming the library, never a silent no-op: a
/// typo that plays nothing looks exactly like a speaker that is off, and the
/// author has no way to tell the two apart.
pub(crate) fn sound(
    call: &str,
    name: &str,
    volume: Option<f32>,
    range: Option<f32>,
) -> Result<PrimSound, String> {
    let sound = SoundName::from_slug(name).ok_or_else(|| {
        format!(
            "{call}: no sound called \"{name}\" — the sounds are {}",
            SoundName::catalogue()
        )
    })?;
    Ok(PrimSound {
        sound,
        volume: finite(volume.unwrap_or(1.0))?,
        range_m: finite(range.unwrap_or(DEFAULT_SOUND_RANGE_M))?,
    }
    .sanitized())
}

/// Read `set_tone{}`'s fields. Only the pitch is required.
pub(crate) fn tone(t: &dyn Rules) -> Result<ToneSpec, String> {
    only_keys(
        t,
        "set_tone field",
        &["pitch", "gain", "voice", "glide", "vibrato", "range"],
    )?;
    let d = ToneSpec::default();
    let voice = match t.text("voice")? {
        None => d.voice,
        Some(v) => Voice::from_slug(&v).ok_or_else(|| {
            format!(
                "set_tone: no voice called \"{v}\" — the voices are {}",
                Voice::catalogue()
            )
        })?,
    };
    let num = |key: &str, fallback: f32| -> Result<f32, String> {
        t.num(key)?.map_or(Ok(fallback), finite)
    };
    let pitch_hz = match t.pitch("pitch")? {
        Pitch::Hz(hz) => finite(hz)?,
        Pitch::Note(text) => note_hz(&text).ok_or_else(|| {
            format!(
                "set_tone: pitch = \"{text}\" is not a note — write it as \"A4\", \"C#3\" or \"Eb5\", or give hertz"
            )
        })?,
        Pitch::Missing => return Err("set_tone: needs a pitch, in hertz or as a note like \"A4\"".to_string()),
        Pitch::Other => return Err("set_tone: pitch must be a note like \"A4\" or a number of hertz".to_string()),
    };
    Ok(ToneSpec {
        voice,
        pitch_hz,
        gain: num("gain", d.gain)?,
        glide_s: num("glide", d.glide_s)?,
        vibrato: num("vibrato", d.vibrato)?,
        range_m: num("range", d.range_m)?,
    }
    .sanitized())
}

impl Rules for mlua::Table {
    fn keys(&self) -> Result<Vec<String>, String> {
        let mut out = Vec::new();
        for pair in self.clone().pairs::<mlua::Value, mlua::Value>() {
            let (k, _) = pair.map_err(|e| e.to_string())?;
            out.push(
                k.as_string()
                    .and_then(|s| s.to_str().ok().map(|v| v.to_string()))
                    .unwrap_or_default(),
            );
        }
        Ok(out)
    }
    fn num(&self, key: &str) -> Result<Option<f32>, String> {
        self.get::<Option<f32>>(key).map_err(|e| e.to_string())
    }
    fn rgb(&self, key: &str) -> Result<Option<[Option<f32>; 3]>, String> {
        let Some(c) = self
            .get::<Option<mlua::Table>>(key)
            .map_err(|e| e.to_string())?
        else {
            return Ok(None);
        };
        let at = |i: i64| c.get::<Option<f32>>(i).map_err(|e| e.to_string());
        Ok(Some([at(1)?, at(2)?, at(3)?]))
    }
    fn text(&self, key: &str) -> Result<Option<String>, String> {
        self.get::<Option<String>>(key).map_err(|e| e.to_string())
    }
    fn pitch(&self, key: &str) -> Result<Pitch, String> {
        Ok(
            match self.get::<mlua::Value>(key).map_err(|e| e.to_string())? {
                mlua::Value::Integer(i) => Pitch::Hz(i as f32),
                mlua::Value::Number(n) => Pitch::Hz(n as f32),
                mlua::Value::String(s) => {
                    Pitch::Note(s.to_str().map_err(|e| e.to_string())?.to_string())
                }
                mlua::Value::Nil => Pitch::Missing,
                _ => Pitch::Other,
            },
        )
    }
}

impl Rules for serde_json::Map<String, serde_json::Value> {
    fn keys(&self) -> Result<Vec<String>, String> {
        Ok(self.keys().cloned().collect())
    }
    fn num(&self, key: &str) -> Result<Option<f32>, String> {
        match self.get(key) {
            None | Some(serde_json::Value::Null) => Ok(None),
            Some(serde_json::Value::Number(n)) => Ok(n.as_f64().map(|v| v as f32)),
            Some(_) => Err(format!("`{key}` must be a number")),
        }
    }
    fn rgb(&self, key: &str) -> Result<Option<[Option<f32>; 3]>, String> {
        match self.get(key) {
            None | Some(serde_json::Value::Null) => Ok(None),
            Some(serde_json::Value::Array(c)) => {
                let at = |i: usize| {
                    c.get(i)
                        .and_then(serde_json::Value::as_f64)
                        .map(|v| v as f32)
                };
                Ok(Some([at(0), at(1), at(2)]))
            }
            Some(_) => Err(format!("`{key}` must be a list of three numbers")),
        }
    }
    fn text(&self, key: &str) -> Result<Option<String>, String> {
        match self.get(key) {
            None | Some(serde_json::Value::Null) => Ok(None),
            Some(serde_json::Value::String(s)) => Ok(Some(s.clone())),
            Some(_) => Err(format!("`{key}` must be text")),
        }
    }
    fn pitch(&self, key: &str) -> Result<Pitch, String> {
        Ok(match self.get(key) {
            Some(serde_json::Value::Number(n)) => {
                n.as_f64().map_or(Pitch::Other, |v| Pitch::Hz(v as f32))
            }
            Some(serde_json::Value::String(s)) => Pitch::Note(s.clone()),
            None | Some(serde_json::Value::Null) => Pitch::Missing,
            Some(_) => Pitch::Other,
        })
    }
}
