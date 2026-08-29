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
//! Names follow Second Life's `ll*` convention with a `vf` prefix: a flat
//! namespace of `verbNoun` calls, `vfSetPos` beside `llSetPos`. That is a
//! deliberate borrowing — the people most likely to script here have twenty years
//! of muscle memory for it, and a gratuitously different spelling of the same idea
//! buys nothing.
//!
//! What is *not* here is as deliberate. Luau already ships `math`, `string`,
//! `table` and `os`, so there is no `vfFrand` (that is `math.random`) and no
//! `vfGetUnixTime` (that is `os.time`). A host function that only forwards to the
//! standard library is a second name for one thing, which is how a reference
//! becomes a place names go to drift apart.

/// Which part of the model a call belongs to. Used to group the editor's
/// reference; purely presentational.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptGroup {
    /// Where the prim is and how it is turned.
    Movement,
    /// What it looks like.
    Appearance,
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
            Self::Appearance => "Appearance",
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
            Self::Appearance,
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
        summary: "Every interval set with vfSetTimerEvent.",
        group: ScriptGroup::Timing,
    },
    ScriptFn {
        name: "on_message",
        signature: "on_message(channel, text, sender_prim_id)",
        summary: "Another script in this region sent something on a channel this \
                  one is listening to.",
        group: ScriptGroup::Messaging,
    },
];

/// Every function installed into a script's environment.
pub const SCRIPT_API: &[ScriptFn] = &[
    // ── Movement ──
    ScriptFn {
        name: "vfGetPos",
        signature: "vfGetPos() -> x, y, z",
        summary: "This prim's position in metres.",
        group: ScriptGroup::Movement,
    },
    ScriptFn {
        name: "vfSetPos",
        signature: "vfSetPos(x, y, z)",
        summary: "Move it. Height is durable; horizontal motion lasts until the \
                  map re-anchors (the geo anchor owns x/z).",
        group: ScriptGroup::Movement,
    },
    ScriptFn {
        name: "vfGetGeo",
        signature: "vfGetGeo() -> latitude, longitude",
        summary: "Where this prim is on Earth, in degrees.",
        group: ScriptGroup::Movement,
    },
    ScriptFn {
        name: "vfSetGeo",
        signature: "vfSetGeo(latitude, longitude)",
        summary: "Move it across the map, durably — this writes the prim's own \
                  geographic anchor, so unlike vfSetPos's x/z it survives a \
                  re-anchor and a restart. Height stays where vfSetPos put it.",
        group: ScriptGroup::Movement,
    },
    ScriptFn {
        name: "vfGeoScale",
        signature: "vfGeoScale(latitude) -> metres_per_degree_lat, metres_per_degree_lng",
        summary: "The local scale of the map at a latitude, so metres can be \
                  turned into degrees: lat + north/mlat, lng + east/mlng.",
        group: ScriptGroup::Movement,
    },
    ScriptFn {
        name: "vfGetRot",
        signature: "vfGetRot() -> rx, ry, rz",
        summary: "Euler rotation in degrees, the same numbers the edit dialog shows.",
        group: ScriptGroup::Movement,
    },
    ScriptFn {
        name: "vfSetRot",
        signature: "vfSetRot(rx, ry, rz)",
        summary: "Turn it. Degrees, not radians.",
        group: ScriptGroup::Movement,
    },
    ScriptFn {
        name: "vfGetScale",
        signature: "vfGetScale() -> x, y, z",
        summary: "Size in metres per axis.",
        group: ScriptGroup::Movement,
    },
    ScriptFn {
        name: "vfSetScale",
        signature: "vfSetScale(x, y, z)",
        summary: "Resize, clamped to 0.01–64 m.",
        group: ScriptGroup::Movement,
    },
    // ── Appearance ──
    ScriptFn {
        name: "vfGetColor",
        signature: "vfGetColor() -> r, g, b",
        summary: "Tint, each channel 0–1.",
        group: ScriptGroup::Appearance,
    },
    ScriptFn {
        name: "vfSetColor",
        signature: "vfSetColor(r, g, b)",
        summary: "Set the tint; values outside 0–1 are clamped.",
        group: ScriptGroup::Appearance,
    },
    ScriptFn {
        name: "vfGetAlpha",
        signature: "vfGetAlpha() -> a",
        summary: "Opacity, 0 (invisible) to 1 (solid).",
        group: ScriptGroup::Appearance,
    },
    ScriptFn {
        name: "vfSetAlpha",
        signature: "vfSetAlpha(a)",
        summary: "Set opacity. SL's transparency % is (1 - a) * 100.",
        group: ScriptGroup::Appearance,
    },
    ScriptFn {
        name: "vfGetGlow",
        signature: "vfGetGlow() -> g",
        summary: "Surface glow, 0–1.",
        group: ScriptGroup::Appearance,
    },
    ScriptFn {
        name: "vfSetGlow",
        signature: "vfSetGlow(g)",
        summary: "Make it emit light of its own, independent of the sun.",
        group: ScriptGroup::Appearance,
    },
    ScriptFn {
        name: "vfSetFullBright",
        signature: "vfSetFullBright(on)",
        summary: "Render fully lit whatever the time of day.",
        group: ScriptGroup::Appearance,
    },
    ScriptFn {
        name: "vfParticles",
        signature: "vfParticles(rules)",
        summary: "Attach a particle emitter, described by a table of named fields \
                  (count, life, speed, spread, gravity, radius, size, end_size, \
                  color, end_color, alpha, turbulence). Anything left out keeps \
                  the fire preset's value; vfParticles(nil) puts the emitter out.",
        group: ScriptGroup::Appearance,
    },
    ScriptFn {
        name: "vfParticleBurst",
        signature: "vfParticleBurst(rules)",
        summary: "The same table, fired once instead of continuously — an \
                  explosion. Defaults to the blast preset rather than the fire one.",
        group: ScriptGroup::Appearance,
    },
    // ── Chat ──
    ScriptFn {
        name: "vfLog",
        signature: "vfLog(text)",
        summary: "Write to the script console. Nobody in-world hears this.",
        group: ScriptGroup::Chat,
    },
    ScriptFn {
        name: "vfWhisper",
        signature: "vfWhisper(text)",
        summary: "Say it in local chat, to 10 m.",
        group: ScriptGroup::Chat,
    },
    ScriptFn {
        name: "vfSay",
        signature: "vfSay(text)",
        summary: "Say it in local chat, to 50 m.",
        group: ScriptGroup::Chat,
    },
    ScriptFn {
        name: "vfShout",
        signature: "vfShout(text)",
        summary: "Say it in local chat, to 300 m.",
        group: ScriptGroup::Chat,
    },
    // ── Timing ──
    ScriptFn {
        name: "vfSetTimerEvent",
        signature: "vfSetTimerEvent(seconds)",
        summary: "Call on_timer every `seconds` (minimum 0.05). Zero cancels it.",
        group: ScriptGroup::Timing,
    },
    ScriptFn {
        name: "vfSleep",
        signature: "vfSleep(seconds)",
        summary: "Suspend this handler and continue later. Only inside a handler.",
        group: ScriptGroup::Timing,
    },
    ScriptFn {
        name: "vfGetTime",
        signature: "vfGetTime() -> seconds",
        summary: "Seconds of simulated time since the script started or was reset.",
        group: ScriptGroup::Timing,
    },
    ScriptFn {
        name: "vfResetTime",
        signature: "vfResetTime()",
        summary: "Put vfGetTime back to zero.",
        group: ScriptGroup::Timing,
    },
    // ── Sensing ──
    ScriptFn {
        name: "vfSensor",
        signature: "vfSensor(range_m) -> avatar_id, name, distance_m",
        summary: "The nearest avatar within range, or nothing at all if there is \
                  none. Measured on the ground, between the prim's geographic \
                  anchor and the avatar's — an avatar whose client has not said \
                  where it is yet is not seen.",
        group: ScriptGroup::Sensing,
    },
    // ── Messaging ──
    ScriptFn {
        name: "vfSend",
        signature: "vfSend(channel, text)",
        summary: "Say something to the other scripts in this region on a numeric \
                  channel. Nobody in-world hears it, and the sender does not hear \
                  itself. This is the only way one prim can reach another.",
        group: ScriptGroup::Messaging,
    },
    ScriptFn {
        name: "vfListen",
        signature: "vfListen(channel [, on])",
        summary: "Start hearing a channel as on_message; vfListen(channel, false) \
                  stops. A script hears nothing until it asks.",
        group: ScriptGroup::Messaging,
    },
    // ── Prim / lifecycle ──
    ScriptFn {
        name: "vfGetPrimId",
        signature: "vfGetPrimId() -> id",
        summary: "This prim's id — the number the editor's title shows.",
        group: ScriptGroup::Lifecycle,
    },
    ScriptFn {
        name: "vfGetName",
        signature: "vfGetName() -> name",
        summary: "This prim's name.",
        group: ScriptGroup::Lifecycle,
    },
    ScriptFn {
        name: "vfSetName",
        signature: "vfSetName(name)",
        summary: "Rename it. This is the name vfSay speaks under.",
        group: ScriptGroup::Lifecycle,
    },
    ScriptFn {
        name: "vfDie",
        signature: "vfDie()",
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

    #[test]
    fn names_are_unique_and_prefixed() {
        let mut seen = HashSet::new();
        for f in SCRIPT_API {
            assert!(
                f.name.starts_with("vf"),
                "{} is not vf-prefixed — the whole point of the naming",
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
