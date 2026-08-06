//! Bevy-free map geodata: MVT decode and rasterisation, road classification, tree
//! scattering, and the routing graph.
//!
//! Everything here is a pure function over bytes and geometry. That is the crate's
//! whole contract — see `Cargo.toml` for why it is enforced by a crate boundary
//! rather than a convention.
//!
//! `viberfox` re-exports these four modules from `systems`, so client code still
//! writes `systems::vector_tiles` and does not name this crate directly.

pub mod mip;
pub mod road_texture;
pub mod furniture;
pub mod routing;
pub mod scatter;
pub mod vector_tiles;
