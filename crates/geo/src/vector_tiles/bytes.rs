use super::*;

pub fn render_mvt(bytes: &[u8], sub: SubTile) -> Result<Vec<u8>, String> {
    super::render_mvt(&TileData::parse(bytes)?, sub)
}

pub fn tessellate_surfaces(bytes: &[u8], tile_m: f32) -> Result<Vec<SurfaceMesh>, String> {
    super::tessellate_surfaces(&TileData::parse(bytes)?, tile_m, None)
}

/// The **ground** sheet only. A test that wants the stretches riding a deck
/// asks [`roads_and_decks`] instead, which is also the one that hands back the
/// carved slabs.
pub fn tessellate_roads(
    bytes: &[u8],
    tile_m: f32,
    decks: &[BridgeDeck],
) -> Result<Vec<SurfaceMesh>, String> {
    let mut decks = decks.to_vec();
    Ok(super::tessellate_roads(&TileData::parse(bytes)?, tile_m, &mut decks, None, false)?.ground)
}

/// The whole answer: the road sheet, the on-deck stretches, and the decks as
/// the cut left them.
pub fn roads_and_decks(
    bytes: &[u8],
    tile_m: f32,
    decks: &[BridgeDeck],
) -> Result<(RoadTessellation, Vec<BridgeDeck>), String> {
    let mut decks = decks.to_vec();
    let roads = super::tessellate_roads(&TileData::parse(bytes)?, tile_m, &mut decks, None, false)?;
    Ok((roads, decks))
}

pub fn tessellate_bridges(bytes: &[u8], tile_m: f32) -> Result<Vec<BridgeDeck>, String> {
    super::tessellate_bridges(&TileData::parse(bytes)?, tile_m)
}

pub fn extract_street_labels(bytes: &[u8], tile_m: f32) -> Result<Vec<StreetLabelLine>, String> {
    super::extract_street_labels(&TileData::parse(bytes)?, tile_m)
}

pub fn ocean_mask(bytes: &[u8], sub: SubTile) -> Result<Vec<u8>, String> {
    super::ocean_mask(&TileData::parse(bytes)?, sub)
}

pub fn water_shore_field(bytes: &[u8], tile_m: f32) -> Result<Option<ShoreField>, String> {
    super::water_shore_field(&TileData::parse(bytes)?, tile_m)
}
