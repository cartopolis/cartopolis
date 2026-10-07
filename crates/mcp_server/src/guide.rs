//! The `scripting_guide` tool's text.

/// What a script can do, for an agent that has no repository beside it.
///
/// The calls come from `cartopolis_core::script_api`, the catalogue the server's
/// own test holds to exactly the functions it installs — so this cannot list one
/// that does not exist. The beacon is included verbatim because a working script
/// is worth more than a list of functions, and it is the one the Grote Markt
/// already runs.
#[must_use]
pub fn scripting_guide() -> String {
    use cartopolis_core::script_api::{api_in_group, ScriptGroup, SCRIPT_EVENTS};
    let mut out = String::from(
        "# Scripting objects\n\n\
         Objects run Luau (a sandboxed Lua). A script is event handlers plus calls such as `say` and `set_color`, \
         and every call acts on the object the script is in: another object is reached \
         only by messages on a channel. Luau's own `math`, `string`, `table` and `os` are \
         there too.\n\n## Events (define the ones you need)\n",
    );
    for f in SCRIPT_EVENTS {
        out.push_str(&format!("- `{}` — {}\n", f.signature, f.summary));
    }
    for group in ScriptGroup::all() {
        out.push_str(&format!("\n## {}\n", group.label()));
        for f in api_in_group(*group) {
            out.push_str(&format!("- `{}` — {}\n", f.signature, f.summary));
        }
    }
    out.push_str(
        "\n## Worth knowing before you build\n\
         - Place objects horizontally with `set_geo`; `set_pos` only keeps its height.\n\
         - Nothing can be shown to one person only: speech is heard by everyone in range, \
         and names, colours and particles are seen by everyone. A card game with hidden \
         hands cannot be built this way.\n\
         - A long loop is paused rather than killed, but it costs the object its turn; use \
         timers instead of busy loops.\n\n\
         ## A working example: the beacon on the Grote Markt\n\n```lua\n",
    );
    out.push_str(include_str!("../../simulator/content/beacon.lua"));
    out.push_str("```\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_guide_carries_a_real_script() {
        let guide = scripting_guide();
        assert!(guide.contains("function on_touch"));
        assert!(guide.contains("set_geo("));
    }
}
