//! cartopolis: what this process has asked the GPU to hold, and how much it has
//! written to it.
//!
//! Counted here, at wgpu's API, so one number covers every caller (bevy's mesh
//! slabs, uniform buffers, render targets, our own textures) on every backend:
//! Vulkan, Metal, DX12, GL and the browser's WebGPU. It is what the application
//! asked for: alignment, padding and the driver's own copies come on top, and
//! surface (swap-chain) textures are not counted, because the platform owns them.
//!
//! Residency counts a buffer or texture from its creation until its last handle is
//! dropped or `destroy` is called, whichever is first. Writes are `write_buffer`,
//! `write_buffer_with`, `write_texture`, `copy_external_image_to_texture`, and a
//! buffer created mapped (`create_buffer_init`), counted at its full size.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

static BUFFERS: AtomicU64 = AtomicU64::new(0);
static BUFFER_BYTES: AtomicU64 = AtomicU64::new(0);
static TEXTURES: AtomicU64 = AtomicU64::new(0);
static TEXTURE_BYTES: AtomicU64 = AtomicU64::new(0);
static PEAK_BYTES: AtomicU64 = AtomicU64::new(0);
static WRITTEN_BYTES: AtomicU64 = AtomicU64::new(0);
static WRITES: AtomicU64 = AtomicU64::new(0);
static CREATED_BYTES: AtomicU64 = AtomicU64::new(0);
static FREED_BYTES: AtomicU64 = AtomicU64::new(0);

/// The counters at one moment. The `*_bytes` residency fields are current; the
/// rest are totals since the process started, so two snapshots subtract.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GpuMeter {
    /// Live buffers.
    pub buffers: u64,
    /// Their bytes.
    pub buffer_bytes: u64,
    /// Live textures.
    pub textures: u64,
    /// Their bytes: every mip, layer and sample.
    pub texture_bytes: u64,
    /// The most buffers and textures ever held at once.
    pub peak_bytes: u64,
    /// Bytes written to the GPU since startup.
    pub written_bytes: u64,
    /// Writes since startup.
    pub writes: u64,
    /// Bytes of buffers and textures created since startup.
    pub created_bytes: u64,
    /// Bytes of buffers and textures freed since startup.
    pub freed_bytes: u64,
}

impl GpuMeter {
    /// Buffers and textures together.
    #[must_use]
    pub fn resident_bytes(&self) -> u64 {
        self.buffer_bytes + self.texture_bytes
    }
}

/// Read the counters.
#[must_use]
pub fn snapshot() -> GpuMeter {
    GpuMeter {
        buffers: BUFFERS.load(Relaxed),
        buffer_bytes: BUFFER_BYTES.load(Relaxed),
        textures: TEXTURES.load(Relaxed),
        texture_bytes: TEXTURE_BYTES.load(Relaxed),
        peak_bytes: PEAK_BYTES.load(Relaxed),
        written_bytes: WRITTEN_BYTES.load(Relaxed),
        writes: WRITES.load(Relaxed),
        created_bytes: CREATED_BYTES.load(Relaxed),
        freed_bytes: FREED_BYTES.load(Relaxed),
    }
}

#[derive(Debug, Clone, Copy)]
enum Kind {
    Buffer,
    Texture,
}

/// One buffer's or texture's share of the residency, held by every clone of its
/// handle and given back once: on `destroy` or when the last clone drops.
#[derive(Debug)]
pub(crate) struct Allocation {
    bytes: u64,
    kind: Kind,
    released: AtomicBool,
}

impl Allocation {
    fn new(bytes: u64, kind: Kind) -> Arc<Self> {
        let (count, total) = match kind {
            Kind::Buffer => (&BUFFERS, &BUFFER_BYTES),
            Kind::Texture => (&TEXTURES, &TEXTURE_BYTES),
        };
        count.fetch_add(1, Relaxed);
        total.fetch_add(bytes, Relaxed);
        CREATED_BYTES.fetch_add(bytes, Relaxed);
        let resident = BUFFER_BYTES.load(Relaxed) + TEXTURE_BYTES.load(Relaxed);
        PEAK_BYTES.fetch_max(resident, Relaxed);
        Arc::new(Self {
            bytes,
            kind,
            released: AtomicBool::new(false),
        })
    }

    /// A buffer of `bytes`. A buffer created mapped is a write of its whole size.
    pub(crate) fn buffer(bytes: u64, mapped_at_creation: bool) -> Arc<Self> {
        if mapped_at_creation {
            written(bytes);
        }
        Self::new(bytes, Kind::Buffer)
    }

    /// A texture as `desc` describes it.
    pub(crate) fn texture(desc: &crate::TextureDescriptor<'_>) -> Arc<Self> {
        Self::new(texture_bytes(desc), Kind::Texture)
    }

    /// A handle that holds no counted memory: a surface texture.
    pub(crate) fn none() -> Arc<Self> {
        Arc::new(Self {
            bytes: 0,
            kind: Kind::Texture,
            released: AtomicBool::new(true),
        })
    }

    /// Give the memory back, once.
    pub(crate) fn release(&self) {
        if self.released.swap(true, Relaxed) {
            return;
        }
        let (count, total) = match self.kind {
            Kind::Buffer => (&BUFFERS, &BUFFER_BYTES),
            Kind::Texture => (&TEXTURES, &TEXTURE_BYTES),
        };
        count.fetch_sub(1, Relaxed);
        total.fetch_sub(self.bytes, Relaxed);
        FREED_BYTES.fetch_add(self.bytes, Relaxed);
    }
}

impl Drop for Allocation {
    fn drop(&mut self) {
        self.release();
    }
}

/// `bytes` handed to the GPU by a write.
pub(crate) fn written(bytes: u64) {
    WRITTEN_BYTES.fetch_add(bytes, Relaxed);
    WRITES.fetch_add(1, Relaxed);
}

/// Every mip level, array layer and sample of a texture, in bytes. Block-compressed
/// formats count whole blocks. A format with no defined block size (the depth and
/// stencil formats) counts four bytes a texel, which is what most drivers spend.
fn texture_bytes(desc: &crate::TextureDescriptor<'_>) -> u64 {
    let (bw, bh) = desc.format.block_dimensions();
    let block = u64::from(desc.format.block_copy_size(None).unwrap_or(4));
    let mut total = 0u64;
    for level in 0..desc.mip_level_count {
        let Some(size) = desc.mip_level_size(level) else {
            break;
        };
        // Array layers for a 1D or 2D texture, depth slices (halving per mip) for 3D.
        let depth = u64::from(size.depth_or_array_layers);
        total += u64::from(size.width.div_ceil(bw))
            * u64::from(size.height.div_ceil(bh))
            * block
            * depth;
    }
    total * u64::from(desc.sample_count.max(1))
}
