//! Print picture resources with their sizes and frame counts.
use openkontakt::Instrument;

fn png_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 24 || &bytes[1..4] != b"PNG" {
        return None;
    }
    let width = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
    let height = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
    Some((width, height))
}

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: picture_dump <instrument.nki>");
    let instrument = Instrument::load(&path).expect("instrument should parse");
    let filter = std::env::args().nth(2);
    let assets = instrument.picture_assets();
    println!("{} picture assets", assets.len());
    for asset in assets {
        if let Some(filter) = &filter {
            if !asset
                .stem
                .to_ascii_lowercase()
                .contains(&filter.to_ascii_lowercase())
            {
                continue;
            }
        }
        let size = png_size(&asset.image);
        if let Ok(dir) = std::env::var("OPENKONTAKT_DUMP_PICTURES") {
            let path = format!("{dir}/{}.png", asset.stem);
            let _ = std::fs::write(path, &asset.image);
        }
        println!(
            "{:<28} {:?} frames_text={:?}",
            asset.stem,
            size,
            asset.text.as_deref().map(str::trim)
        );
    }
}
