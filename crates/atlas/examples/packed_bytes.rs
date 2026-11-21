//! What packing a JSON cell file saves, measured against the live host:
//! `cargo run -p cartopolis_atlas --example packed_bytes -- <url>…`

use std::io::Read;

fn br(bytes: &[u8]) -> usize {
    let params = brotli::enc::BrotliEncoderParams {
        quality: 10,
        lgwin: 22,
        size_hint: bytes.len(),
        ..Default::default()
    };
    let mut out = Vec::new();
    brotli::BrotliCompress(&mut &bytes[..], &mut out, &params).unwrap();
    out.len()
}

fn main() {
    for url in std::env::args().skip(1) {
        let mut text = String::new();
        ureq::get(&url)
            .call()
            .unwrap()
            .into_reader()
            .read_to_string(&mut text)
            .unwrap();
        let started = std::time::Instant::now();
        let packed = cartopolis_geo::formats::packed::pack_or_why(&text);
        let pack_ms = started.elapsed().as_millis();
        match packed {
            Ok(p) => {
                let started = std::time::Instant::now();
                let back = cartopolis_geo::formats::packed::unpack(&p).unwrap();
                let unpack_ms = started.elapsed().as_millis();
                let same = serde_json::from_str::<serde_json::Value>(&text).unwrap()
                    == serde_json::from_str::<serde_json::Value>(&back).unwrap();
                println!(
                    "{url}\n  json {} br {} | packed {} br {} ({} % of json's br) | pack {pack_ms} ms unpack {unpack_ms} ms same {same}",
                    text.len(),
                    br(text.as_bytes()),
                    p.len(),
                    br(&p),
                    100 * br(&p) / br(text.as_bytes()).max(1)
                );
            }
            Err(why) => println!("{url}\n  not packed: {why}"),
        }
    }
}
