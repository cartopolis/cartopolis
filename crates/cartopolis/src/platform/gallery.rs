//! Putting a finished picture or clip where the person keeps their photos:
//! the phone's gallery, or a desktop's Pictures and Videos folders. The
//! camera's Save is the caller (`systems::camera_screen`).
//!
//! **Not `platform::storage`, on purpose.** That store is the app's own
//! persistence — keyed blobs under the app's directories, gone with the app.
//! This is the opposite: a file handed to the person, which must outlive the
//! app and show up where their other photos are. On Android that is a
//! MediaStore row (`Pictures/Cartopolis`, the same place the saved places'
//! photos go, and `Movies/Cartopolis` for clips); on a desktop it is a plain
//! file in `~/Pictures/Cartopolis` or `~/Videos/Cartopolis`.
//! Not compiled for the web, which has no camera and so no caller.

/// Where saved pictures land, as a person would look for them.
pub const FOLDER: &str = "Pictures/Cartopolis";

/// Where saved clips land on Android: its own name for the videos folder.
#[cfg(target_os = "android")]
pub const VIDEO_FOLDER: &str = "Movies/Cartopolis";

/// What is being saved: which folder, and on Android which MediaStore
/// collection and type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A JPEG picture.
    Picture,
    /// A WebM clip.
    Clip,
}

/// Save `jpeg` as `name` (a file name, `.jpg`) and say where it went. Blocks
/// on the write — and on Android possibly on a permission dialog — so call it
/// from a worker thread.
pub fn save_jpeg(name: &str, jpeg: &[u8]) -> Result<String, String> {
    save(name, jpeg, Kind::Picture)
}

/// Save a clip, `webm`, as `name` (`.webm`), as [`save_jpeg`] saves a picture.
pub fn save_video(name: &str, webm: &[u8]) -> Result<String, String> {
    save(name, webm, Kind::Clip)
}

fn save(name: &str, bytes: &[u8], kind: Kind) -> Result<String, String> {
    #[cfg(target_os = "android")]
    {
        crate::platform::android::save_to_gallery(name, bytes, kind)?;
        Ok(match kind {
            Kind::Picture => FOLDER,
            Kind::Clip => VIDEO_FOLDER,
        }
        .to_string())
    }
    #[cfg(not(target_os = "android"))]
    {
        // `~/Pictures` (or `~/Videos`) where there is a home, else beside the
        // app's own data so the file is at least somewhere that can be found.
        let folder = match kind {
            Kind::Picture => FOLDER,
            Kind::Clip => "Videos/Cartopolis",
        };
        let dir = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(|home| std::path::PathBuf::from(home).join(folder))
            .unwrap_or_else(|| crate::utils::cache::app_data_root().join("pictures"));
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let path = dir.join(name);
        std::fs::write(&path, bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        tracing::info!(path = %path.display(), ?kind, "saved to the gallery");
        Ok(path.display().to_string())
    }
}
