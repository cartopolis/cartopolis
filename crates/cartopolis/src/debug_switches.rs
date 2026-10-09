//! The bisecting switches (`CARTO_NO_*` and friends) and settings values: read
//! from the environment on desktop, the query string on the web and system
//! properties on Android, with the persisted Settings preference as the fallback.

#[cfg(target_os = "android")]
use crate::platform;
use crate::resources;

/// Startup-time feature switch read from the environment. Used for the render
/// subsystems that can't be safely toggled at runtime (atmosphere, env-map IBL):
/// removing those components mid-run tears down in-flight GPU resources and hangs
/// the device, so they're chosen once at launch. `CARTO_NO_ATMOSPHERE=1`,
/// `CARTO_NO_ENVMAP=1`, `CARTO_NO_SHADOWS=1`.
///
/// Shadows are also a live dev-panel toggle, so the flag looks redundant — it is
/// not. The dev panel is unreachable in a scripted run, and shadows turned out to
/// be the single largest slice of submitted geometry (~79% of triangles), which
/// makes "measure it without them" the first question anyone profiling the web
/// build wants to ask. The flag only forces them *off*; the panel still governs
/// the rest.
///
/// A browser has no environment, so on wasm these are read from the page's query
/// string instead (ADR-033 already puts web configuration there). Without it every
/// one of these switches is silently dead on the web — `std::env::var_os` simply
/// always returns `None` — and they are switches whose whole purpose is to bisect a
/// slow or broken renderer, which is exactly the situation the web build is in.
/// `?CARTO_NO_ATMOSPHERE=1`, or the tidier `?no_atmosphere` (the `CARTO_`
/// prefix and the case are both optional in a URL, where there is no environment
/// to collide with).
///
/// **Android has no environment either**, and its version of the problem is worse
/// than the browser's: a NativeActivity is started by the zygote and inherits
/// `app_process`'s environment, and there is no shell on the phone to add to it.
/// So every switch here was silently dead on the one platform whose renderer is
/// hardest to bisect — the platform where you cannot attach a debugger, cannot
/// read stderr and get one build per CI run. There the source is a **system
/// property**: `CARTO_NO_ATMOSPHERE` → `debug.carto.no_atmosphere`, set with
/// `adb shell setprop` and read at startup. See
/// [`platform::android::system_property`] for why `debug.` specifically, and for
/// the rest of the recipe.
/// The persisted half of [`env_flag`], seeded once by [`init_debug_switches`].
///
/// A process-wide `OnceLock` and not a `Resource` for the same reason
/// `quality::budget()` is one: the earliest reader runs before the `App` exists
/// (`CARTO_NO_DUAL_SOURCE` is consulted while `DefaultPlugins` is still being
/// built), so there is no world to hold it.
static DEBUG_SWITCHES: std::sync::OnceLock<resources::DebugSwitches> = std::sync::OnceLock::new();

/// Seed the persisted switches. Called once at the top of `run_client`, beside
/// `quality::init` and for the same reason: everything downstream expects it
/// settled, and the first reader is earlier than it looks.
///
/// Silently keeps the first value if called twice, as `quality::init` does — a
/// second answer would mean two different runs' switches inside one process.
pub fn init_debug_switches(switches: resources::DebugSwitches) {
    let _ = DEBUG_SWITCHES.set(switches);
}

/// **Two sources, and the explicit one wins.** Since 2026-08-23 these switches are
/// also a persisted preference (`resources::DebugSwitches`, Settings ▸ Graphics ▸
/// Developer), because on Android the source below is `adb`, i.e. a cable and a
/// second machine — and the Adreno sky bug spent four days with its own instrument
/// built and unreachable by the person holding the device. An env var or property
/// that is *present* still decides, in both directions: it is an instruction for
/// this launch and is never written back, exactly as `--connect` is against the
/// remembered session. The preference is only what a launch falls back to.
///
/// Resolving it here rather than at the call sites is the point: every switch in
/// the codebase became reachable from the phone without touching one of them.
pub fn env_flag(name: &str) -> bool {
    resolve_flag(
        env_flag_explicit(name),
        DEBUG_SWITCHES.get().and_then(|switches| switches.get(name)),
    )
}

/// The resolution [`env_flag`] performs, as a function so it can be tested.
///
/// Its own function for the reason `frame_pacing::resolve_frame_rate` records:
/// the rung test there stayed green about a number the call site had stopped
/// passing on, because what was asserted was the input and not the resolution. A
/// `OnceLock` and a process environment are both untestable in a unit test; this
/// is the part with a rule in it.
fn resolve_flag(explicit: Option<bool>, preference: Option<bool>) -> bool {
    explicit.or(preference).unwrap_or(false)
}

/// The platform's own answer: `Some` when the switch is *present*, `None` when it
/// is absent and [`env_flag`] should fall back to the preference.
///
/// The three-way answer is what makes `CARTO_X=0` mean "off" rather than "not
/// set" — without it, a property set to `0` to overrule a preference would fall
/// straight through to that preference and appear to do nothing, which is the
/// worst possible behaviour for a debugging switch.
fn env_flag_explicit(name: &str) -> Option<bool> {
    #[cfg(target_os = "android")]
    {
        // Android has no environment either, and for a harder reason than the
        // browser's: the zygote hands a NativeActivity `app_process`'s own
        // environment and there is no shell on the device to add to it. So the
        // same switches come from system properties instead — see
        // `platform::android::system_property`, which is also where the
        // `adb shell setprop` recipe lives.
        //
        // An *empty* property is the absence of one: `getprop` cannot tell them
        // apart, and `setprop … ""` is how a property is cleared.
        return platform::android::system_property(name)
            .filter(|v| !v.is_empty())
            .map(|v| v != "0");
    }
    #[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
    {
        // An empty environment variable is idiomatically unset, and was already
        // read as false here; it is now read as *absent*, which is the same thing
        // one layer up unless a preference says otherwise.
        std::env::var_os(name)
            .filter(|v| !v.is_empty())
            .map(|v| v != "0")
    }
    #[cfg(target_arch = "wasm32")]
    {
        query_flag(name)
    }
}

/// The wasm half of [`env_flag`]: look `name` up in `window.location.search`.
///
/// Deliberately *not* identical to the environment's rule about empty values. An
/// empty environment variable is idiomatically unset, so `env_flag` treats it as
/// false; a bare `?no_atmosphere` in a URL just as plainly means "on", and typing
/// `=1` to satisfy a convention borrowed from a different platform would be a
/// papercut on every debugging URL. Presence is truth here; `=0` and `=false` are
/// the ways to say otherwise.
/// `None` when the key is absent from the query string — see
/// [`env_flag_explicit`] for why absence and `=0` must be different answers.
#[cfg(target_arch = "wasm32")]
fn query_flag(name: &str) -> Option<bool> {
    query_value(name).map(|v| v.is_empty() || (v != "0" && !v.eq_ignore_ascii_case("false")))
}

/// A settings *value* (not a switch), read from the environment on desktop and
/// from the page's query string on the web — the same two sources, and the same
/// naming rules, as [`env_flag`]. `CARTO_PLACE="53.2, 6.5, 120m"` on desktop,
/// `?place=53.2,6.5,120m` in the browser.
pub fn env_value(name: &str) -> Option<String> {
    #[cfg(target_os = "android")]
    {
        // See `env_flag`: system properties stand in for the environment.
        return platform::android::system_property(name).filter(|v| !v.is_empty());
    }
    #[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
    {
        std::env::var(name).ok().filter(|v| !v.is_empty())
    }
    #[cfg(target_arch = "wasm32")]
    {
        query_value(name).filter(|v| !v.is_empty())
    }
}

/// `CARTO_NO_ATMOSPHERE` → `debug.carto.no_atmosphere`: the Android half of
/// [`env_value`]'s name mapping.
///
/// **Deliberately not inside `platform::android`**, and not `cfg`-gated, although
/// it is only ever *called* from there. That module is
/// `cfg(target_os = "android")`; no CI lane builds Android tests, and `cargo
/// check` — which is all the Android lane runs — does not compile `cfg(test)`
/// code at all. A test next to the caller would therefore never execute on any
/// machine, and this is precisely the function whose failure is silent: a name
/// that maps to a property nobody sets reads as "unset" forever, so a typo here
/// looks exactly like a switch that does not work. Here it is covered by the
/// ordinary `cargo test -p cartopolis` run.
///
/// The `debug.` prefix is load-bearing rather than decorative — it is the one
/// property namespace `adb shell setprop` may write on a **production** build
/// without root, which is what makes this reachable on a user's own phone.
// `allow(dead_code)` off Android, against the workspace's `unused = "deny"`: the
// sole non-test caller is `platform::android`, which does not exist on this
// target, so here the tests below are the only users — which is exactly the
// arrangement this function was moved out of that module to get.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub(crate) fn android_property_name(name: &str) -> String {
    let short = name.strip_prefix("CARTO_").unwrap_or(name);
    format!("debug.carto.{}", short.to_ascii_lowercase())
}

#[cfg(test)]
mod flag_resolution_tests {
    use super::resolve_flag;
    use crate::resources::DebugSwitches;

    /// A switch nobody has expressed an opinion about is off, which is what every
    /// call site assumed when the environment was the only source.
    #[test]
    fn an_unmentioned_switch_is_off() {
        assert!(!resolve_flag(None, None));
    }

    /// The preference is what a launch falls back to — the whole point of the
    /// feature, since on Android there is nothing else to fall back to.
    #[test]
    fn the_preference_decides_when_nothing_is_set() {
        assert!(resolve_flag(None, Some(true)));
        assert!(!resolve_flag(None, Some(false)));
    }

    /// **An explicit setting wins in both directions, and the second half is the
    /// one worth a test.** `CARTO_X=1` beating a preference is obvious;
    /// `CARTO_X=0` beating a preference that is *on* is what stops `setprop
    /// debug.carto.no_haze 0` from silently doing nothing, which for a
    /// debugging switch is the worst behaviour available.
    #[test]
    fn an_explicit_setting_overrides_the_preference() {
        assert!(resolve_flag(Some(true), Some(false)));
        assert!(!resolve_flag(Some(false), Some(true)));
    }

    /// Every switch the Settings section offers must survive the property-name
    /// mapping, because on the platform this exists for that mapping is the only
    /// way anyone else can reproduce what the user set.
    #[test]
    fn every_settings_switch_is_reachable_as_a_property() {
        for (name, _, _) in DebugSwitches::SWITCHES {
            let mapped = super::android_property_name(name);
            assert!(
                mapped.starts_with("debug.carto."),
                "{name} maps to {mapped}"
            );
            assert!(mapped.len() <= 31, "{mapped} is {} bytes", mapped.len());
        }
    }
}

#[cfg(test)]
mod android_property_tests {
    use super::android_property_name;

    #[test]
    fn a_carto_variable_maps_to_a_debug_property() {
        assert_eq!(
            android_property_name("CARTO_NO_ATMOSPHERE"),
            "debug.carto.no_atmosphere"
        );
        assert_eq!(
            android_property_name("CARTO_FPS_LOG"),
            "debug.carto.fps_log"
        );
        assert_eq!(
            android_property_name("CARTO_QUALITY"),
            "debug.carto.quality"
        );
    }

    /// The prefix is optional, so a name that never carried it still lands in our
    /// own namespace rather than loose in the platform's.
    #[test]
    fn an_unprefixed_name_is_still_namespaced() {
        assert_eq!(android_property_name("QUALITY"), "debug.carto.quality");
    }

    /// A property *name* was capped at 31 bytes before Android 8. We target 28+,
    /// where it is not — but the cap is the kind of thing that would truncate
    /// silently on an older handset, and every switch this is called with fits it
    /// anyway, so the cheap thing is to keep it that way.
    #[test]
    fn every_switch_fits_the_legacy_name_limit() {
        for name in [
            "CARTO_NO_ATMOSPHERE",
            "CARTO_NO_HAZE",
            "CARTO_NO_ENVMAP",
            "CARTO_NO_OCEAN",
            "CARTO_NO_CLOUDS",
            "CARTO_NO_SHADOWS",
            "CARTO_NO_SPACE_RAYMARCH",
            "CARTO_NO_DUAL_SOURCE",
            "CARTO_WEB_LIMITS",
            "CARTO_FPS_LOG",
            "CARTO_FPS_CAP",
            "CARTO_QUALITY",
            "CARTO_RENDER_SCALE",
            "CARTO_RENDER_COST",
            "CARTO_TILE_URL",
            "CARTO_PLACE",
            "CARTO_UPLOAD_KB",
        ] {
            let mapped = android_property_name(name);
            assert!(mapped.len() <= 31, "{mapped} is {} bytes", mapped.len());
        }
    }
}

/// Look `name` up in `window.location.search`, accepting the `CARTO_`-prefixed
/// spelling and the bare one, in either case.
#[cfg(target_arch = "wasm32")]
fn query_value(name: &str) -> Option<String> {
    let search = web_sys::window().and_then(|w| w.location().search().ok())?;
    let params = web_sys::UrlSearchParams::new_with_str(&search).ok()?;
    let short = name.strip_prefix("CARTO_").unwrap_or(name);
    [name, short]
        .iter()
        .flat_map(|k| [k.to_ascii_uppercase(), k.to_ascii_lowercase()])
        .find_map(|k| params.get(&k))
}
