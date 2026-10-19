//! cartopolis: compiled WGSL kept between runs.
//!
//! Composing a WESL shader to WGSL is CPU work, and on the web it runs on the
//! page's only thread: a few hundred milliseconds per new pipeline variant, and
//! about two seconds over a load (`docs/notes/wasm-performance.md`). The output
//! is a pure function of its inputs, so the cache asks a [`CompiledShaderStore`]
//! the application installed before it composes anything, and hands it every
//! shader it composes.
//!
//! What a stored shader is keyed by: the main module's path and source, the
//! feature flags and constants it was composed with, and [`FORMAT`]. Which other
//! modules it pulled in is not known until it has been composed, so a stored
//! shader lists them, each by asset path and source hash, and a lookup only hits
//! when every one of them is still registered with the same source.

use alloc::{string::String, vec::Vec};
use std::sync::OnceLock;

/// Bumped whenever the composition itself changes: the wesl version, the
/// compiler options bevy passes, or anything else that changes output for the
/// same inputs.
pub const FORMAT: &str = "bevy_shader 0.20.0 / wesl 0.6.0 / strip imports condcomp / v1";

/// A composed shader, and the modules it was composed from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredShader {
    /// The WGSL handed to the renderer.
    pub wgsl: String,
    /// `(asset path, source hash)` of every registered module the composition
    /// read, the main module included.
    pub modules: Vec<(String, u64)>,
}

/// Where composed shaders are kept. Installed once with
/// [`set_compiled_shader_store`]; without one, every shader is composed as usual.
pub trait CompiledShaderStore: Send + Sync + 'static {
    /// The shader composed under `key`, if it is known. A stale answer is safe:
    /// the cache checks its module list before using it.
    fn get(&self, key: u64) -> Option<StoredShader>;
    /// A shader just composed under `key`.
    fn put(&self, key: u64, shader: StoredShader);
}

static STORE: OnceLock<Box<dyn CompiledShaderStore>> = OnceLock::new();

/// Install the store. Only the first call wins; returns whether this one did.
pub fn set_compiled_shader_store(store: Box<dyn CompiledShaderStore>) -> bool {
    STORE.set(store).is_ok()
}

pub(crate) fn store() -> Option<&'static dyn CompiledShaderStore> {
    STORE.get().map(AsRef::as_ref)
}

/// FNV-1a, 64-bit: stable across runs, platforms and builds, which the hashers
/// in `std` and `bevy_platform` are not promised to be.
#[derive(Clone, Copy)]
pub(crate) struct StableHasher(u64);

impl StableHasher {
    pub(crate) fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }

    pub(crate) fn bytes(&mut self, bytes: &[u8]) -> &mut Self {
        for b in bytes {
            self.0 ^= u64::from(*b);
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
        self
    }

    /// A field, length-prefixed so that `("ab", "c")` and `("a", "bc")` differ.
    pub(crate) fn field(&mut self, bytes: &[u8]) -> &mut Self {
        self.bytes(&(bytes.len() as u64).to_le_bytes()).bytes(bytes)
    }

    pub(crate) fn finish(&self) -> u64 {
        self.0
    }
}

/// The hash a module's source is listed under.
pub(crate) fn source_hash(source: &[u8]) -> u64 {
    StableHasher::new().bytes(source).finish()
}
