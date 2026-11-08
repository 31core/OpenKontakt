//! What a `.nkr` archive actually contains.
use openkontakt::archive::parse_resource_archive;

fn main() {
    let path = std::env::args().nth(1).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let archive = parse_resource_archive(&bytes);
    println!("{} entries", archive.entries.len());
    for entry in archive.entries.iter().take(40) {
        println!(
            "  {:<40} {:>9} B  png={}",
            entry.name,
            entry.data.len(),
            entry.data.starts_with(b"\x89PNG")
        );
    }
    println!(
        "pictures: {:?}",
        archive
            .pictures()
            .iter()
            .map(|picture| picture.stem.clone())
            .collect::<Vec<_>>()
    );
}
