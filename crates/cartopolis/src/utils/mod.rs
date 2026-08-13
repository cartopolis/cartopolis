// Resolving a cache *directory* only means something where there is a filesystem
// to put one on: the web build's half of `platform::storage` is the browser's own
// store, so on wasm every item in here is dead code.
#[cfg(not(target_arch = "wasm32"))]
pub mod cache;
pub mod clipboard;
// Decoding and re-encoding a place's photo is pure byte work with no filesystem
// in it, so unlike `cache` above this compiles on every target — the web build
// captures and shows photos too, it just has nowhere durable to put them.
pub mod photo;
pub mod photo_store;
pub mod sun;
pub mod tile_utils;
