//! Putting a finished picture where the person keeps their photos: the
//! phone's gallery, or a desktop's Pictures folder. The field check's "Save to
//! phone" is the caller (`systems::field_check`).
//!
//! **Not `platform::storage`, on purpose.** That store is the app's own
//! persistence — keyed blobs under the app's directories, gone with the app.
//! This is the opposite: a file handed to the person, which must outlive the
//! app and show up where their other photos are. On Android that is a
//! MediaStore row (`Pictures/Cartopolis`, the same place the saved places'
//! photos go); on a desktop it is a plain file in `~/Pictures/Cartopolis`.
//! Not compiled for the web, which has no field check and so no caller.

/// Where saved pictures land, as a person would look for them.
pub const FOLDER: &str = "Pictures/Cartopolis";

/// Save `jpeg` as `name` (a file name, `.jpg`) and say where it went. Blocks
/// on the write — and on Android possibly on a permission dialog — so call it
/// from a worker thread.
pub fn save_jpeg(name: &str, jpeg: &[u8]) -> Result<String, String> {
    #[cfg(target_os = "android")]
    {
        crate::platform::android::save_to_gallery(name, jpeg)?;
        Ok(FOLDER.to_string())
    }
    #[cfg(not(target_os = "android"))]
    {
        // `~/Pictures` where there is a home, else beside the app's own data so
        // the picture is at least somewhere that can be found.
        let dir = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(|home| std::path::PathBuf::from(home).join(FOLDER))
            .unwrap_or_else(|| crate::utils::cache::app_data_root().join("pictures"));
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let path = dir.join(name);
        std::fs::write(&path, jpeg).map_err(|e| format!("{}: {e}", path.display()))?;
        tracing::info!(path = %path.display(), "saved a picture");
        Ok(path.display().to_string())
    }
}
