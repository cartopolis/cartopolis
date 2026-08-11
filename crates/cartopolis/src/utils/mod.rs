// Resolving a cache *directory* only means something where there is a filesystem
// to put one on: the web build's half of `platform::storage` is the browser's own
// store, so on wasm every item in here is dead code.
#[cfg(not(target_arch = "wasm32"))]
pub mod cache;
pub mod clipboard;
pub mod sun;
pub mod tile_utils;
