//! The script API's *catalogue* — one row per callable, shared by the server that
//! implements them and the editor that documents them.
//!
//! It lives in `cartopolis_core` for the same reason `PROTOCOL_HISTORY` does: two
//! crates have to agree, and the way they stay in agreement is that there is only
//! one list. The simulator has a test
//! (`scripting::tests::the_catalogue_matches_the_installed_api`) asserting that a
//! freshly compiled script sees *exactly* these names — no undocumented function,
//! no documented-but-missing one. A reference that drifts is worse than none,
//! because an author trusts it.
//!
//! Names are a flat namespace of plain `verb_noun` calls — `set_pos`, `say`,
//! `play_sound`. Second Life's *shape* (one global per verb, `set_pos` where it
//! has `llSetPos`) without its prefix: the `vf` prefix the API had until
//! 2026-10-04 was a brand name standing in front of every call, and it outlived
//! the brand. snake_case because that is what Luau's own library uses
//! (`string.format`, `table.insert`), so a script reads as one language.
//!
//! What is *not* here is as deliberate. Luau already ships `math`, `string`,
//! `table` and `os`, so there is no `frand` (that is `math.random`) and no
//! `get_unix_time` (that is `os.time`). A host function that only forwards to the
//! standard library is a second name for one thing, which is how a reference
//! becomes a place names go to drift apart.

/// Which part of the model a call belongs to. Used to group the editor's
/// reference; purely presentational.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptGroup {
    /// Where the prim is and how it is turned.
    Movement,
    /// Being a physical object: falling, bouncing, being pushed.
    Physics,
    /// What it looks like.
    Appearance,
    /// Making a noise.
    Sound,
    /// Saying things — to the world, or to the script console.
    Chat,
    /// Events over time, and suspending a handler.
    Timing,
    /// Noticing what is around the prim.
    Sensing,
    /// Talking to the other scripts in the region.
    Messaging,
    /// Facts about the prim, and ending it.
    Lifecycle,
}

impl ScriptGroup {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Movement => "Movement",
            Self::Physics => "Physics",
            Self::Appearance => "Appearance",
            Self::Sound => "Sound",
            Self::Chat => "Chat",
            Self::Timing => "Timing",
            Self::Sensing => "Sensing",
            Self::Messaging => "Messages",
            Self::Lifecycle => "Prim",
        }
    }

    /// Every group, in the order the reference shows them.
    #[must_use]
    pub fn all() -> &'static [ScriptGroup] {
        &[
            Self::Movement,
            Self::Physics,
            Self::Appearance,
            Self::Sound,
            Self::Chat,
            Self::Timing,
            Self::Sensing,
            Self::Messaging,
            Self::Lifecycle,
        ]
    }
}

/// One callable a script can reach.
#[derive(Debug, Clone, Copy)]
pub struct ScriptFn {
    pub name: &'static str,
    /// Argument list and return, as an author would write it.
    pub signature: &'static str,
    pub summary: &'static str,
    pub group: ScriptGroup,
}

/// The handlers a script may define. Not calls — the host looks for these by name
/// and runs them when the matching thing happens.
pub const SCRIPT_EVENTS: &[ScriptFn] = &[
    ScriptFn {
        name: "on_start",
        signature: "on_start()",
        summary: "Once, when the script is stored or the region comes back up.",
        group: ScriptGroup::Timing,
    },
    ScriptFn {
        name: "on_touch",
        signature: "on_touch(avatar_id, avatar_name)",
        summary: "Someone touched this prim.",
        group: ScriptGroup::Timing,
    },
    ScriptFn {
        name: "on_timer",
        signature: "on_timer()",
        summary: "Every interval set with set_timer_event.",
        group: ScriptGroup::Timing,
    },
    ScriptFn {
        name: "on_message",
        signature: "on_message(channel, text, sender_prim_id)",
        summary: "Another script in this region sent something on a channel this \
                  one is listening to.",
        group: ScriptGroup::Messaging,
    },
    ScriptFn {
        name: "on_hand",
        signature: "on_hand(avatar_id, avatar_name, x, y, z, precise)",
        summary: "Somebody's hand is near this prim (sense_hands): metres from its \
                  centre in its own frame (turned with it, not scaled), x right, y \
                  up, z back. Up to 20 times a second per person while their hand \
                  is in range; only each person's newest is kept if the script \
                  falls behind. Only people who switched on high-precision tracking \
                  are sensed, so precise is true — false only for a developer \
                  testing from a desktop camera. A hand that stops arriving has \
                  left.",
        group: ScriptGroup::Sensing,
    },
];

/// Every function installed into a script's environment.
pub const SCRIPT_API: &[ScriptFn] = &[
    // ── Movement ──
    ScriptFn {
        name: "get_pos",
        signature: "get_pos() -> x, y, z",
        summary: "This prim's position in metres.",
        group: ScriptGroup::Movement,
    },
    ScriptFn {
        name: "set_pos",
        signature: "set_pos(x, y, z)",
        summary: "Move it. Height is durable; horizontal motion lasts until the \
                  map re-anchors (the geo anchor owns x/z).",
        group: ScriptGroup::Movement,
    },
    ScriptFn {
        name: "get_geo",
        signature: "get_geo() -> latitude, longitude",
        summary: "Where this prim is on Earth, in degrees.",
        group: ScriptGroup::Movement,
    },
    ScriptFn {
        name: "set_geo",
        signature: "set_geo(latitude, longitude)",
        summary: "Move it across the map, durably — this writes the prim's own \
                  geographic anchor, so unlike set_pos's x/z it survives a \
                  re-anchor and a restart. Height stays where set_pos put it.",
        group: ScriptGroup::Movement,
    },
    ScriptFn {
        name: "geo_scale",
        signature: "geo_scale(latitude) -> metres_per_degree_lat, metres_per_degree_lng",
        summary: "The local scale of the map at a latitude, so metres can be \
                  turned into degrees: lat + north/mlat, lng + east/mlng.",
        group: ScriptGroup::Movement,
    },
    ScriptFn {
        name: "get_rot",
        signature: "get_rot() -> rx, ry, rz",
        summary: "Euler rotation in degrees, the same numbers the edit dialog shows.",
        group: ScriptGroup::Movement,
    },
    ScriptFn {
        name: "set_rot",
        signature: "set_rot(rx, ry, rz)",
        summary: "Turn it. Degrees, not radians.",
        group: ScriptGroup::Movement,
    },
    ScriptFn {
        name: "get_scale",
        signature: "get_scale() -> x, y, z",
        summary: "Size in metres per axis.",
        group: ScriptGroup::Movement,
    },
    ScriptFn {
        name: "set_scale",
        signature: "set_scale(x, y, z)",
        summary: "Resize, clamped to 0.01–64 m.",
        group: ScriptGroup::Movement,
    },
    // ── Physics ──
    ScriptFn {
        name: "set_physical",
        signature: "set_physical(on)",
        summary: "Make this prim a physical object, which falls, bounces off \
                  other prims and is pushed by avatars, or make it stay put again.",
        group: ScriptGroup::Physics,
    },
    ScriptFn {
        name: "get_physical",
        signature: "get_physical() -> on",
        summary: "Whether this prim is a physical object.",
        group: ScriptGroup::Physics,
    },
    ScriptFn {
        name: "get_vel",
        signature: "get_vel() -> x, y, z",
        summary: "How fast it is moving, in metres per second (x east, y up, \
                  z south). Zero for a prim that is not physical.",
        group: ScriptGroup::Physics,
    },
    ScriptFn {
        name: "set_vel",
        signature: "set_vel(x, y, z)",
        summary: "Set how fast it is moving, in metres per second. Only a \
                  physical prim moves; capped at 60 m/s.",
        group: ScriptGroup::Physics,
    },
    ScriptFn {
        name: "apply_impulse",
        signature: "apply_impulse(x, y, z)",
        summary: "Give it a shove, in kilogram-metres per second: a heavy prim \
                  moves less for the same push. Only a physical prim moves.",
        group: ScriptGroup::Physics,
    },
    ScriptFn {
        name: "get_mass",
        signature: "get_mass() -> kilograms",
        summary: "What it weighs, from its size and shape.",
        group: ScriptGroup::Physics,
    },
    // ── Appearance ──
    ScriptFn {
        name: "get_color",
        signature: "get_color() -> r, g, b",
        summary: "Tint, each channel 0–1.",
        group: ScriptGroup::Appearance,
    },
    ScriptFn {
        name: "set_color",
        signature: "set_color(r, g, b)",
        summary: "Set the tint; values outside 0–1 are clamped.",
        group: ScriptGroup::Appearance,
    },
    ScriptFn {
        name: "get_alpha",
        signature: "get_alpha() -> a",
        summary: "Opacity, 0 (invisible) to 1 (solid).",
        group: ScriptGroup::Appearance,
    },
    ScriptFn {
        name: "set_alpha",
        signature: "set_alpha(a)",
        summary: "Set opacity. SL's transparency % is (1 - a) * 100.",
        group: ScriptGroup::Appearance,
    },
    ScriptFn {
        name: "get_glow",
        signature: "get_glow() -> g",
        summary: "Surface glow, 0–1.",
        group: ScriptGroup::Appearance,
    },
    ScriptFn {
        name: "set_glow",
        signature: "set_glow(g)",
        summary: "Make it emit light of its own, independent of the sun.",
        group: ScriptGroup::Appearance,
    },
    ScriptFn {
        name: "set_full_bright",
        signature: "set_full_bright(on)",
        summary: "Render fully lit whatever the time of day.",
        group: ScriptGroup::Appearance,
    },
    ScriptFn {
        name: "particles",
        signature: "particles(rules)",
        summary: "Attach a particle emitter, described by a table of named fields \
                  (count, life, speed, spread_deg, gravity, source_radius, \
                  size_start, size_end, color_start, color_end, alpha, \
                  turbulence) — the same names the admin API takes. Anything left \
                  out keeps the fire preset's value; particles(nil) puts the \
                  emitter out.",
        group: ScriptGroup::Appearance,
    },
    ScriptFn {
        name: "particle_burst",
        signature: "particle_burst(rules)",
        summary: "The same table, fired once instead of continuously — an \
                  explosion. Defaults to the blast preset rather than the fire one.",
        group: ScriptGroup::Appearance,
    },
    // ── Sound ──
    ScriptFn {
        name: "play_sound",
        signature: "play_sound(name, volume?, range_m?)",
        summary: "Play a sound once from this prim. Names: firework, crackle, pop, \
                  chime, bell, click, whoosh. Volume 0–1 (default 1); heard out to \
                  range_m metres (default 50, at most 300). An unknown name is an error.",
        group: ScriptGroup::Sound,
    },
    ScriptFn {
        name: "loop_sound",
        signature: "loop_sound(name, volume?, range_m?)",
        summary: "Play a sound over and over until stop_sound. One loop per prim: \
                  calling it again replaces the loop. Not kept over a server \
                  restart, so start a standing loop in on_start.",
        group: ScriptGroup::Sound,
    },
    ScriptFn {
        name: "stop_sound",
        signature: "stop_sound()",
        summary: "Stop this prim's looping sound.",
        group: ScriptGroup::Sound,
    },
    ScriptFn {
        name: "set_tone",
        signature: "set_tone{pitch, gain?, voice?, glide?, vibrato?, range?}",
        summary: "Sound a synthesised tone from this prim until stop_tone, replacing \
                  any tone already sounding. pitch is hertz or a note (\"A4\", \"C#3\"); \
                  gain 0–1 (default 0.5); voice: sine, organ, saw, square; glide: \
                  seconds to slide to a new pitch (default 0.05); vibrato 0–1; heard \
                  out to range metres (default 50). Call it as often as the tone \
                  should change — from on_hand to follow a hand.",
        group: ScriptGroup::Sound,
    },
    ScriptFn {
        name: "stop_tone",
        signature: "stop_tone()",
        summary: "Silence this prim's tone.",
        group: ScriptGroup::Sound,
    },
    // ── Chat ──
    ScriptFn {
        name: "log",
        signature: "log(text)",
        summary: "Write to the script console. Nobody in-world hears this.",
        group: ScriptGroup::Chat,
    },
    ScriptFn {
        name: "whisper",
        signature: "whisper(text)",
        summary: "Say it in local chat, to 10 m.",
        group: ScriptGroup::Chat,
    },
    ScriptFn {
        name: "say",
        signature: "say(text)",
        summary: "Say it in local chat, to 50 m.",
        group: ScriptGroup::Chat,
    },
    ScriptFn {
        name: "shout",
        signature: "shout(text)",
        summary: "Say it in local chat, to 300 m.",
        group: ScriptGroup::Chat,
    },
    // ── Timing ──
    ScriptFn {
        name: "set_timer_event",
        signature: "set_timer_event(seconds)",
        summary: "Call on_timer every `seconds` (minimum 0.05). Zero cancels it.",
        group: ScriptGroup::Timing,
    },
    ScriptFn {
        name: "sleep",
        signature: "sleep(seconds)",
        summary: "Suspend this handler and continue later. Only inside a handler.",
        group: ScriptGroup::Timing,
    },
    ScriptFn {
        name: "get_time",
        signature: "get_time() -> seconds",
        summary: "Seconds of simulated time since the script started or was reset.",
        group: ScriptGroup::Timing,
    },
    ScriptFn {
        name: "reset_time",
        signature: "reset_time()",
        summary: "Put get_time back to zero.",
        group: ScriptGroup::Timing,
    },
    // ── Sensing ──
    ScriptFn {
        name: "sensor",
        signature: "sensor(range_m) -> avatar_id, name, distance_m",
        summary: "The nearest avatar within range, or nothing at all if there is \
                  none. Measured on the ground, between the prim's geographic \
                  anchor and the avatar's — an avatar whose client has not said \
                  where it is yet is not seen.",
        group: ScriptGroup::Sensing,
    },
    ScriptFn {
        name: "sense_hands",
        signature: "sense_hands(range_m)",
        summary: "Ask for on_hand from everybody whose hand comes within range_m of \
                  this prim (at most 5). sense_hands(0) stops. Not kept over a \
                  restart, so call it in on_start.",
        group: ScriptGroup::Sensing,
    },
    // ── Messaging ──
    ScriptFn {
        name: "send",
        signature: "send(channel, text)",
        summary: "Say something to the other scripts in this region on a numeric \
                  channel. Nobody in-world hears it, and the sender does not hear \
                  itself. This is the only way one prim can reach another.",
        group: ScriptGroup::Messaging,
    },
    ScriptFn {
        name: "listen",
        signature: "listen(channel [, on])",
        summary: "Start hearing a channel as on_message; listen(channel, false) \
                  stops. A script hears nothing until it asks.",
        group: ScriptGroup::Messaging,
    },
    // ── Prim / lifecycle ──
    ScriptFn {
        name: "get_prim_id",
        signature: "get_prim_id() -> id",
        summary: "This prim's id — the number the editor's title shows.",
        group: ScriptGroup::Lifecycle,
    },
    ScriptFn {
        name: "get_name",
        signature: "get_name() -> name",
        summary: "This prim's name.",
        group: ScriptGroup::Lifecycle,
    },
    ScriptFn {
        name: "set_name",
        signature: "set_name(name)",
        summary: "Rename it. This is the name say speaks under.",
        group: ScriptGroup::Lifecycle,
    },
    ScriptFn {
        name: "die",
        signature: "die()",
        summary: "Delete this prim, and the script with it. There is no undo.",
        group: ScriptGroup::Lifecycle,
    },
];

/// Every callable in a group, in catalogue order.
#[must_use]
pub fn api_in_group(group: ScriptGroup) -> impl Iterator<Item = &'static ScriptFn> {
    SCRIPT_API.iter().filter(move |f| f.group == group)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// Luau's own globals. A host function named like one of these would hide it
    /// from every script.
    const LUAU_GLOBALS: &[&str] = &[
        "assert",
        "error",
        "getmetatable",
        "ipairs",
        "next",
        "pairs",
        "pcall",
        "print",
        "rawequal",
        "rawget",
        "rawlen",
        "rawset",
        "require",
        "select",
        "setmetatable",
        "tonumber",
        "tostring",
        "type",
        "typeof",
        "unpack",
        "xpcall",
        "gcinfo",
        "newproxy",
        "bit32",
        "buffer",
        "coroutine",
        "debug",
        "math",
        "os",
        "string",
        "table",
        "utf8",
        "vector",
    ];

    #[test]
    fn names_are_unique_snake_case_and_clear_of_luau() {
        let mut seen = HashSet::new();
        for f in SCRIPT_API {
            assert!(
                !f.name.is_empty()
                    && f.name
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
                "{} is not snake_case",
                f.name
            );
            assert!(
                !LUAU_GLOBALS.contains(&f.name),
                "{} would hide a Luau global of the same name",
                f.name
            );
            assert!(seen.insert(f.name), "{} is listed twice", f.name);
            assert!(
                f.signature.starts_with(f.name),
                "{}'s signature should start with its own name: {}",
                f.name,
                f.signature
            );
        }
    }

    /// Every entry is reachable through the grouped view the editor renders, so a
    /// call cannot be documented and yet invisible in the reference.
    #[test]
    fn every_call_appears_in_exactly_one_group() {
        let grouped: usize = ScriptGroup::all()
            .iter()
            .map(|g| api_in_group(*g).count())
            .sum();
        assert_eq!(grouped, SCRIPT_API.len());
    }
}
