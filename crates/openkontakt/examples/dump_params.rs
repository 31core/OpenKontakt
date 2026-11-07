use openkontakt::Instrument;
fn main() {
    let path = std::env::args().nth(1).unwrap();
    let instrument = Instrument::load(&path).unwrap();
    let p = &instrument.program.params;
    println!(
        "version=0x{:X} suffix_bytes={}",
        instrument.program.version, p.suffix_bytes
    );
    println!("suffix={:02X?}", p.suffix);
    println!("wallpaper_filename={:?}", p.wallpaper_filename);
    println!(
        "wallpaper_requested={:?} resolved={:?}",
        instrument.wallpaper_requested, instrument.wallpaper_path
    );
    if let Some(table) = &instrument.filename_table {
        println!(
            "table v{} samples={} others={} trailing={}",
            table.version,
            table.sample_count(),
            table.other.len(),
            table.trailing.len()
        );
        for (i, e) in table.other.iter().enumerate() {
            println!("  other[{i}] {:?} segs={:?}", e.display_path(), e.segments);
        }
    }
    println!(
        "panel: {:.0}x{:.0} px",
        instrument.ksp_panel.ui_width_px, instrument.ksp_panel.ui_height_px
    );
    println!("picture source: {:?}", instrument.picture_source());
    for asset in instrument.picture_assets() {
        println!(
            "  picture {:22} {} bytes txt={:?}",
            asset.stem,
            asset.image.len(),
            asset
                .text
                .as_deref()
                .map(|t| t.lines().take(2).collect::<Vec<_>>().join(" | "))
        );
    }
    for s in &instrument.scripts {
        println!(
            "slot {} empty={} textfile={:?} text_len={}",
            s.index,
            s.is_empty(),
            s.textfile_name,
            s.text.as_ref().map(|t| t.len()).unwrap_or(0)
        );
    }
}
