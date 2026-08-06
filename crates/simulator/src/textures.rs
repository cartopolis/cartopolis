use std::collections::HashMap;
use std::fs;
use std::path::Path;
use viberfox_core::TextureEntry;

pub fn list_textures(dir: &str) -> Vec<TextureEntry> {
    let path = Path::new(dir);
    if !path.exists() {
        return Vec::new();
    }

    let manifest = load_manifest(dir);

    let mut seen_stems = std::collections::HashSet::new();
    let mut entries = Vec::new();
    if let Ok(read_dir) = fs::read_dir(path) {
        for entry in read_dir.flatten() {
            let p = entry.path();
            let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
            if !matches!(ext, "png" | "jpg" | "jpeg") {
                continue;
            }
            if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                if !seen_stems.insert(stem.to_string()) {
                    continue; // prefer first file when stem collides across extensions
                }
                let name = manifest.get(stem).cloned().unwrap_or_else(|| title_case(stem));
                entries.push(TextureEntry {
                    id: stem.to_string(),
                    name,
                });
            }
        }
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    entries
}

/// Returns raw PNG bytes for `{dir}/{id}.png`, or `None` if the file is missing.
/// The id is sanitised to prevent path traversal.
pub fn load_texture(dir: &str, id: &str) -> Option<Vec<u8>> {
    let safe_id: String = id
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
        .collect();
    if safe_id.is_empty() {
        return None;
    }
    for ext in &["png", "jpg", "jpeg"] {
        let path = Path::new(dir).join(format!("{safe_id}.{ext}"));
        if let Ok(bytes) = fs::read(&path) {
            return Some(bytes);
        }
    }
    None
}

fn load_manifest(dir: &str) -> HashMap<String, String> {
    let path = Path::new(dir).join("manifest.toml");
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return HashMap::new(),
    };
    let mut map = HashMap::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let key = k.trim().to_string();
            let value = v.trim().trim_matches('"').to_string();
            map.insert(key, value);
        }
    }
    map
}

fn title_case(s: &str) -> String {
    let spaced = s.replace('_', " ").replace('-', " ");
    let mut chars = spaced.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
    }
}
