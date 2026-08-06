//! Self-installs Linux desktop integration so the OS window icon appears with no
//! manual step.
//!
//! On Wayland winit's `set_window_icon` is a no-op, so the compositor derives the
//! taskbar/titlebar icon from a `.desktop` file it
//! matches to the window's app-id (`Window.name = "viberfox"`). Rather than ship a
//! script the user must run first, the app writes that `.desktop` file plus the
//! embedded icon into the per-user XDG dirs at startup — idempotently, and
//! *before* the window is created so the compositor can resolve the icon on the
//! first map. Best-effort: any failure is logged and ignored, never fatal.
//!
//! No-op on non-Linux (winit sets the icon directly there).

/// Ensure the `.desktop` file + themed icon are installed for the current user.
/// Call once in `main`, before building the Bevy app. No-op off Linux.
pub fn ensure_desktop_integration() {
    #[cfg(target_os = "linux")]
    linux::ensure();
}

#[cfg(target_os = "linux")]
mod linux {
    use std::env;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use crate::systems::window_icon::ICON_PNG;

    /// `$XDG_DATA_HOME`, or `$HOME/.local/share`.
    fn data_home() -> Option<PathBuf> {
        env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
    }

    /// The `.desktop` entry. `Exec` points at *this* binary so a menu/taskbar
    /// launch runs the same build; `StartupWMClass` matches `Window.name` so the
    /// compositor pairs the running window to this entry.
    fn desktop_contents() -> String {
        let exec = env::current_exe()
            .ok()
            .and_then(|p| p.to_str().map(str::to_owned))
            .map(|p| {
                if p.contains(char::is_whitespace) {
                    format!("\"{p}\"")
                } else {
                    p
                }
            })
            .unwrap_or_else(|| "viberfox".to_owned());
        format!(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name=Viberfox\n\
             Comment=Planet-scale OSM/3DBAG world viewer\n\
             Exec={exec}\n\
             Icon=viberfox\n\
             Terminal=false\n\
             Categories=Graphics;\n\
             StartupWMClass=viberfox\n"
        )
    }

    /// Write `bytes` to `path` only if absent or different. Returns whether the
    /// file was (re)written. Creates parent dirs.
    fn write_if_changed(path: &Path, bytes: &[u8]) -> std::io::Result<bool> {
        if fs::read(path).is_ok_and(|cur| cur == bytes) {
            return Ok(false);
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, bytes)?;
        Ok(true)
    }

    pub(super) fn ensure() {
        let Some(data) = data_home() else {
            return;
        };
        let desktop = data.join("applications/viberfox.desktop");
        let icon = data.join("icons/hicolor/256x256/apps/viberfox.png");

        let mut changed = false;
        for (path, bytes) in [
            (&desktop, desktop_contents().into_bytes()),
            (&icon, ICON_PNG.to_vec()),
        ] {
            match write_if_changed(path, &bytes) {
                Ok(wrote) => changed |= wrote,
                // A read-only home or missing HOME is not worth crashing over.
                Err(e) => bevy::log::warn!("desktop integration: {}: {e}", path.display()),
            }
        }

        if changed {
            refresh_caches(&data);
        }
    }

    /// Nudge the desktop caches so a fresh install shows without a relogin.
    /// All best-effort and detached — spawn, never wait, ignore absence.
    fn refresh_caches(data: &Path) {
        let apps = data.join("applications");
        let hicolor = data.join("icons/hicolor");
        let spawn = |cmd: &str, args: &[&str]| {
            let _ = Command::new(cmd).args(args).spawn();
        };
        spawn("update-desktop-database", &[apps.to_str().unwrap_or(".")]);
        spawn(
            "gtk-update-icon-cache",
            &["-f", "-t", hicolor.to_str().unwrap_or(".")],
        );
        // KDE/Plasma reads its own sycoca cache (KService); rebuild so KWin finds
        // the new entry. Version-suffixed on Plasma 6 / 5.
        spawn("kbuildsycoca6", &["--noincremental"]);
        spawn("kbuildsycoca5", &["--noincremental"]);
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn temp_path(tag: &str) -> PathBuf {
            env::temp_dir().join(format!("viberfox-desktop-{}-{tag}", std::process::id()))
        }

        #[test]
        fn write_if_changed_is_idempotent() {
            let path = temp_path("idem");
            let _ = fs::remove_file(&path);

            // Absent → writes.
            assert!(write_if_changed(&path, b"one").unwrap());
            // Identical → skips.
            assert!(!write_if_changed(&path, b"one").unwrap());
            // Different → rewrites.
            assert!(write_if_changed(&path, b"two").unwrap());
            assert_eq!(fs::read(&path).unwrap(), b"two");

            let _ = fs::remove_file(&path);
        }

        #[test]
        fn desktop_entry_matches_the_window_app_id() {
            let s = desktop_contents();
            // The compositor pairs the window to this entry via app-id; the icon
            // name must resolve in the hicolor theme we install into.
            assert!(s.contains("StartupWMClass=viberfox"), "{s}");
            assert!(s.contains("Icon=viberfox"), "{s}");
            assert!(s.starts_with("[Desktop Entry]"), "{s}");
        }
    }
}
