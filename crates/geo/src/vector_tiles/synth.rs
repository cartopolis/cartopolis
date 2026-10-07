/// A base-128 varint.
pub fn varint(mut value: u64) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let byte = (value & 0x7F) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return out;
        }
        out.push(byte | 0x80);
    }
}

/// A length-delimited record: the key, the length, the payload.
pub fn bytes_field(field: u64, payload: &[u8]) -> Vec<u8> {
    let mut out = varint((field << 3) | 2);
    out.extend(varint(payload.len() as u64));
    out.extend_from_slice(payload);
    out
}

/// A varint record.
pub fn varint_field(field: u64, value: u64) -> Vec<u8> {
    let mut out = varint(field << 3);
    out.extend(varint(value));
    out
}

/// Zigzag, which is how a geometry parameter is encoded. The left shift is
/// done on the unsigned bit pattern so that a coordinate near `i32::MAX` —
/// which is the whole point of the hostile fixtures — is plainly a discarded
/// bit rather than something a reader has to convince themselves about.
fn zigzag(value: i32) -> u64 {
    u64::from(((value as u32) << 1) ^ ((value >> 31) as u32))
}

/// One layer holding a single line feature through `points`, in tile-local
/// integer coordinates.
pub fn line_layer(name: &str, extent: u32, points: &[(i32, i32)]) -> Vec<u8> {
    // `MoveTo` for the first point, then one `LineTo` covering all the rest;
    // parameters are deltas from the running cursor.
    let mut geometry = varint(9);
    let (mut cx, mut cy) = (0, 0);
    for (i, &(x, y)) in points.iter().enumerate() {
        if i == 1 {
            geometry.extend(varint(((points.len() as u64 - 1) << 3) | 2));
        }
        geometry.extend(varint(zigzag(x - cx)));
        geometry.extend(varint(zigzag(y - cy)));
        (cx, cy) = (x, y);
    }
    let mut feature = varint_field(3, 2); // type = LINESTRING
    feature.extend(bytes_field(4, &geometry));
    layer(name, extent, &[feature])
}

/// One layer holding a single polygon through `ring` (not repeated at the
/// end), in tile-local integer coordinates. Clockwise in tile space (y
/// down) is an exterior ring, as the MVT specification has it.
pub fn polygon_layer(name: &str, extent: u32, ring: &[(i32, i32)]) -> Vec<u8> {
    let mut geometry = varint(9);
    let (mut cx, mut cy) = (0, 0);
    for (i, &(x, y)) in ring.iter().enumerate() {
        if i == 1 {
            geometry.extend(varint(((ring.len() as u64 - 1) << 3) | 2));
        }
        geometry.extend(varint(zigzag(x - cx)));
        geometry.extend(varint(zigzag(y - cy)));
        (cx, cy) = (x, y);
    }
    geometry.extend(varint(15)); // ClosePath
    let mut feature = varint_field(3, 3); // type = POLYGON
    feature.extend(bytes_field(4, &geometry));
    layer(name, extent, &[feature])
}

/// [`line_layer`] with one string tag on the feature — the shape a
/// Shortbread `streets` feature really has, where the discriminator is a
/// `kind` **value** in the layer's value table rather than the layer's own
/// name. [`super::body_mentions_rail`] is the reader that cares.
pub fn tagged_line_layer(
    name: &str,
    extent: u32,
    (key, value): (&str, &str),
    points: &[(i32, i32)],
) -> Vec<u8> {
    let mut geometry = varint(9);
    let (mut cx, mut cy) = (0, 0);
    for (i, &(x, y)) in points.iter().enumerate() {
        if i == 1 {
            geometry.extend(varint(((points.len() as u64 - 1) << 3) | 2));
        }
        geometry.extend(varint(zigzag(x - cx)));
        geometry.extend(varint(zigzag(y - cy)));
        (cx, cy) = (x, y);
    }
    let mut feature = varint_field(3, 2); // type = LINESTRING
                                          // tags: [key index 0, value index 0], packed.
    let mut tags = varint(0);
    tags.extend(varint(0));
    feature.extend(bytes_field(2, &tags));
    feature.extend(bytes_field(4, &geometry));

    let mut out = bytes_field(1, name.as_bytes());
    out.extend(bytes_field(2, &feature));
    out.extend(bytes_field(3, key.as_bytes()));
    // A `Value` message with its `string_value` (field 1) set.
    out.extend(bytes_field(4, &bytes_field(1, value.as_bytes())));
    out.extend(varint_field(5, u64::from(extent)));
    out.extend(varint_field(15, 2));
    out
}

/// A layer out of already-encoded features.
pub fn layer(name: &str, extent: u32, features: &[Vec<u8>]) -> Vec<u8> {
    let mut out = bytes_field(1, name.as_bytes());
    for feature in features {
        out.extend(bytes_field(2, feature));
    }
    out.extend(varint_field(5, u64::from(extent)));
    // Version is `required`, and mvt-reader refuses anything but 1 or 2.
    out.extend(varint_field(15, 2));
    out
}

/// A tile out of already-encoded layers.
pub fn tile(layers: &[Vec<u8>]) -> Vec<u8> {
    layers.iter().flat_map(|l| bytes_field(3, l)).collect()
}

/// The allocation bomb: `moves` repetitions of the three bytes
/// `[MoveTo(1), 0, 0]`, each of which makes mvt-reader retain one vector
/// sized for the *whole* stream.
pub fn geometry_bomb(moves: usize) -> Vec<u8> {
    let geometry: Vec<u8> = (0..moves).flat_map(|_| [9u8, 0, 0]).collect();
    let mut feature = varint_field(3, 2);
    feature.extend(bytes_field(4, &geometry));
    tile(&[layer("streets", 4096, &[feature])])
}
