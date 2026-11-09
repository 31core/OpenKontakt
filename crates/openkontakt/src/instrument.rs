//! Top-level instrument model: container + Program + groups + zones + names + scripts.

use crate::chunk::walk_chunks;
use crate::error::{Error, Result};
use crate::filename::{FileNameTable, parse_file_name_table, parse_legacy_file_name_list};
use crate::group::{Group, parse_group_list};
use crate::ksp_ui::KspPanel;
use crate::nis::unwrap_nki;
use crate::program::{Program, parse_program};
use crate::reader::Reader;
use crate::resolve::{ResolutionDiagnostic, SampleResolver};
use crate::script::{ScriptSlot, parse_script};
use crate::wav::{AudioSample, open_sample};
use crate::zone::{Zone, parse_zone_list};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiagnosticSeverity {
    Info,
    Warning,
}

#[derive(Clone, Debug)]
pub struct Diagnostic {
    pub severity: DiagnosticSeverity,
    pub area: &'static str,
    pub message: String,
}

/// Where an `other` filename reference actually lives.
///
/// A packed library writes the reference as `<container>.nkr/<path inside the container>` — the
/// instrument's impulse responses are addressed exactly this way, and the `.nkc` catalogue is what
/// turns that path into an offset.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OtherResource {
    /// A loose file on disk.
    File(PathBuf),
    /// A payload inside an `.nkx`/`.nkr`, with the range the catalogue gives for it.
    Packed {
        container: PathBuf,
        start: usize,
        size: usize,
    },
}

#[derive(Clone, Debug)]
pub struct Instrument {
    pub source_path: PathBuf,
    pub name: String,
    pub container: crate::nis::ContainerInfo,
    pub program: Program,
    pub groups: Vec<Group>,
    pub zones: Vec<Zone>,
    pub scripts: Vec<ScriptSlot>,
    pub filename_table: Option<FileNameTable>,
    pub ksp_panel: KspPanel,
    pub library_root: PathBuf,
    pub sample_resolution: HashMap<i32, ResolutionDiagnostic>,
    pub resolved_samples: BTreeMap<i32, PathBuf>,
    /// Samples that exist only inside a `.nkc` catalogue: the entry — and therefore the payload's
    /// offset and length — is known, the `.ncw` codec is not.
    pub packed_samples: HashMap<i32, crate::nkc::NkcEntry>,
    /// Wallpaper resolved from Program's terminal filename reference (F2).
    pub wallpaper_path: Option<PathBuf>,
    pub wallpaper_requested: Option<String>,
    /// `other` entries of the filename table, resolved to files on disk. Index 0 is the
    /// instrument itself; the rest are the wallpaper, the IR samples and anything else the
    /// program references by index (`Program F2`, the convolution module).
    pub resolved_other: Vec<Option<PathBuf>>,
    pub diagnostics: Vec<Diagnostic>,
}

impl Instrument {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let source_path = path.as_ref().to_path_buf();
        let bytes = std::fs::read(&source_path).map_err(|source| Error::Io {
            path: source_path.clone(),
            source,
        })?;
        Self::from_bytes(source_path, &bytes)
    }

    pub fn from_bytes(source_path: PathBuf, bytes: &[u8]) -> Result<Self> {
        let unwrapped = unwrap_nki(bytes)?;
        let chunks = walk_chunks(&unwrapped.chunk_stream)?;
        let mut diagnostics = Vec::new();

        let mut program_chunk = None;
        let mut file_table_chunk = None;
        let mut legacy_file_table_chunk = None;
        for chunk in &chunks {
            match chunk.id {
                0x28 if program_chunk.is_none() => program_chunk = Some(chunk.body),
                0x28 => diagnostics.push(Diagnostic {
                    severity: DiagnosticSeverity::Warning,
                    area: "container",
                    message: "multiple Program chunks; using the first".into(),
                }),
                0x4B if file_table_chunk.is_none() => file_table_chunk = Some(chunk.body),
                0x3D if legacy_file_table_chunk.is_none() => {
                    legacy_file_table_chunk = Some(chunk.body)
                }
                0x3D => {}
                _ => {}
            }
        }

        let program_body = program_chunk.ok_or(Error::NoProgram)?;
        let program = parse_program(program_body)?;

        let groups = if let Some(child) = program.children_of(0x33).next() {
            parse_group_list(&child.body)?
        } else {
            Vec::new()
        };
        let zones = if let Some(child) = program.children_of(0x34).next() {
            parse_zone_list(&child.body)?
        } else {
            Vec::new()
        };

        for zone in &zones {
            if zone.group_index >= groups.len() {
                diagnostics.push(Diagnostic {
                    severity: DiagnosticSeverity::Warning,
                    area: "zones",
                    message: format!(
                        "zone {} references group {}, but GroupList has {} groups",
                        zone.index,
                        zone.group_index,
                        groups.len()
                    ),
                });
            }
        }

        let mut scripts = Vec::new();
        for child in program.children_of(0x06) {
            scripts.push(parse_script(scripts.len(), &child.body)?);
        }
        if scripts.len() > 5 {
            diagnostics.push(Diagnostic {
                severity: DiagnosticSeverity::Warning,
                area: "scripts",
                message: format!(
                    "{} script slots found; Kontakt's layout has five",
                    scripts.len()
                ),
            });
        }

        let (filename_table, table_error) = if let Some(body) = file_table_chunk {
            match parse_file_name_table(body) {
                Ok(table) => (Some(table), None),
                Err(error) => (None, Some(error.to_string())),
            }
        } else if let Some(body) = legacy_file_table_chunk {
            match parse_legacy_file_name_list(body) {
                Ok(table) => (Some(table), None),
                Err(error) => (None, Some(error.to_string())),
            }
        } else {
            (None, None)
        };
        if let Some(error) = table_error {
            diagnostics.push(Diagnostic {
                severity: DiagnosticSeverity::Warning,
                area: "filename table",
                message: format!("filename table not decoded: {error}"),
            });
        }
        if filename_table.is_none() {
            diagnostics.push(Diagnostic {
                severity: DiagnosticSeverity::Warning,
                area: "filename table",
                message: "no usable 0x4B / 0x3D filename table; samples cannot be resolved".into(),
            });
        }

        let mut resolver = SampleResolver::new(&source_path);
        let mut sample_resolution = HashMap::new();
        let mut resolved_samples = BTreeMap::new();
        // A packed library addresses its samples through a `.nkc` catalogue instead of keeping
        // them on disk. Those are *found* — they just cannot be decoded yet, which is a different
        // diagnosis from "this path does not exist".
        let nkc = parse_nkc_index(&source_path, &resolver.library_root);
        let mut packed_samples: HashMap<i32, crate::nkc::NkcEntry> = HashMap::new();
        if let Some(table) = &filename_table {
            for zone in &zones {
                let Some(info) = &zone.sample else {
                    continue;
                };
                if sample_resolution.contains_key(&info.filename_id) {
                    continue;
                }
                let resolved = resolver.resolve(table, info.filename_id);
                if let Some(path) = resolved.path {
                    resolved_samples.insert(info.filename_id, path);
                    continue;
                }
                let name = table
                    .sample(info.filename_id)
                    .and_then(|file| file.segments.iter().rev().find_map(|s| s.text.clone()));
                let in_catalogue = nkc
                    .as_ref()
                    .zip(name.as_ref())
                    .and_then(|(index, name)| index.get(name));
                if let Some(entry) = in_catalogue {
                    packed_samples.insert(info.filename_id, entry.clone());
                    continue;
                }
                if let Some(diagnostic) = resolved.diagnostic {
                    sample_resolution.insert(info.filename_id, diagnostic);
                }
            }
        }

        let archive_count = sample_resolution
            .values()
            .filter(|d| d.kind == crate::resolve::ResolutionDiagnosticKind::ArchiveMember)
            .count();
        let missing_count = sample_resolution
            .values()
            .filter(|d| d.kind == crate::resolve::ResolutionDiagnosticKind::Missing)
            .count();
        if !packed_samples.is_empty() {
            diagnostics.push(Diagnostic {
                severity: DiagnosticSeverity::Warning,
                area: "samples",
                message: format!(
                    "{} sample(s) are indexed in the library's .nkc catalogue; NCW decoding is not \
                     implemented, so those zones are silent",
                    packed_samples.len()
                ),
            });
        }
        if archive_count > 0 {
            diagnostics.push(Diagnostic {
                severity: DiagnosticSeverity::Warning,
                area: "samples",
                message: format!(
                    "{archive_count} sample path(s) point inside NKX/NKR archives; those samples are silent"
                ),
            });
        }
        if missing_count > 0 {
            diagnostics.push(Diagnostic {
                severity: DiagnosticSeverity::Warning,
                area: "samples",
                message: format!(
                    "{missing_count} sample path(s) could not be resolved; those zones are silent"
                ),
            });
        }

        let mut resolved_other: Vec<Option<PathBuf>> = Vec::new();
        if let Some(table) = &filename_table {
            for index in 0..table.other.len() as i32 {
                let Some(entry) = table.wallpaper(index) else {
                    resolved_other.push(None);
                    continue;
                };
                resolved_other.push(resolver.resolve_entry(entry).path);
            }
        }

        let mut wallpaper_path = None;
        let mut wallpaper_requested = None;
        if let (Some(table), Some(reference)) = (&filename_table, program.params.wallpaper_filename)
        {
            if let Some(entry) = table.wallpaper(reference) {
                let requested = entry.display_path();
                if looks_like_image(&requested) {
                    wallpaper_requested = Some(requested.clone());
                    let resolved = resolver.resolve_entry(entry);
                    if resolved.path.is_some() {
                        wallpaper_path = resolved.path;
                    } else if let Some(diagnostic) = resolved.diagnostic {
                        diagnostics.push(Diagnostic {
                            severity: DiagnosticSeverity::Warning,
                            area: "wallpaper",
                            message: diagnostic.message,
                        });
                    }
                }
            } else {
                diagnostics.push(Diagnostic {
                    severity: DiagnosticSeverity::Warning,
                    area: "wallpaper",
                    message: format!(
                        "Program F2 wallpaper reference {reference} is outside the filename table"
                    ),
                });
            }
        }

        let ksp_panel = KspPanel::from_slots(&scripts);

        if program.params.group_solo != 0 && program.params.group_solo != 1 {
            diagnostics.push(Diagnostic {
                severity: DiagnosticSeverity::Info,
                area: "Program",
                message: format!(
                    "group_solo stores {}, which is preserved verbatim",
                    program.params.group_solo
                ),
            });
        }

        Ok(Self {
            source_path,
            name: program.params.name.clone(),
            container: unwrapped.info,
            program,
            groups,
            zones,
            scripts,
            filename_table,
            ksp_panel,
            library_root: resolver.library_root.clone(),
            sample_resolution,
            resolved_samples,
            packed_samples,
            wallpaper_path,
            wallpaper_requested,
            resolved_other,
            diagnostics,
        })
    }

    /// Resolved path of a filename-table `other` entry, which is what the program's wallpaper
    /// reference and an `0x16` convolution module both address.
    pub fn other_path(&self, index: i32) -> Option<&Path> {
        if index < 0 {
            return None;
        }
        self.resolved_other
            .get(index as usize)
            .and_then(Option::as_ref)
            .map(PathBuf::as_path)
    }

    /// Resolve an `other` reference, following it into the resource package when it points there.
    ///
    /// Packed libraries write an impulse response as `../Resources/Resources.nkr/Resources/ir_samples/01_Transform.NCW`:
    /// a container path followed by the path inside it. The `.nkc` catalogue maps that inner path
    /// to an offset and a length, and the payload starts one block header past the record.
    pub fn other_resource(&self, index: i32) -> Option<OtherResource> {
        if index < 0 {
            return None;
        }
        if let Some(path) = self.other_path(index) {
            return Some(OtherResource::File(path.to_path_buf()));
        }
        let entry = self.filename_table.as_ref()?.wallpaper(index)?;
        let (container, inner) = split_packed_reference(&entry.display_path())?;
        let container = if container.is_absolute() {
            // A leading `/` addresses the installed Kontakt's shared content root, not the file
            // system root: factory impulse responses are written
            // `/presets/Effects/Convolution/K4IR.nkx/…` and live under `<content root>/Presets/…`.
            // The stored spelling is lower case while the folder is `Presets`, so the walk below
            // matches each component case-insensitively.
            let relative = container.strip_prefix("/").unwrap_or(&container);
            let root = crate::resolve::factory_content_root()?;
            resolve_case_insensitive(&root, relative)?
        } else {
            let base = self.source_path.parent().unwrap_or(Path::new("."));
            normalise_path(&base.join(&container))
        };
        if !container.is_file() {
            return None;
        }
        let bare = inner.rsplit(['/', '\\']).next()?;
        let catalogue = self.nkc_index()?;
        let record = catalogue.get(bare)?;
        let size = record.size? as usize;
        Some(OtherResource::Packed {
            container,
            start: record.offset as usize + SAMPLE_RECORD_HEADER,
            size,
        })
    }

    pub fn sample_path_for_zone(&self, zone: &Zone) -> Option<&Path> {
        let id = zone.sample.as_ref()?.filename_id;
        self.resolved_samples.get(&id).map(PathBuf::as_path)
    }

    pub fn zone_sample_ids(&self) -> Vec<i32> {
        let mut ids: Vec<i32> = self
            .zones
            .iter()
            .filter_map(|zone| zone.sample.as_ref().map(|s| s.filename_id))
            .filter(|id| *id >= 0)
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    pub fn group_for_zone(&self, zone: &Zone) -> Option<&Group> {
        self.groups.get(zone.group_index)
    }

    /// The keys that actually play, i.e. the union of every zone's key range clipped to the
    /// instrument's Program range. KONTAKT paints exactly these keys with `$KEY_COLOR_DEFAULT`,
    /// its standard blue for mapped notes (unmapped keys stay black and white); scripts can
    /// override the color per key with `set_key_color()`.
    pub fn mapped_keys(&self) -> [bool; 128] {
        mapped_key_mask(
            self.zones.iter().map(|zone| (zone.low_key, zone.high_key)),
            self.program.params.low_key,
            self.program.params.high_key,
        )
    }

    pub fn stats(&self) -> InstrumentStats {
        let with_samples = self.zones.iter().filter(|z| z.sample.is_some()).count();
        InstrumentStats {
            groups: self.groups.len(),
            zones: self.zones.len(),
            zones_with_samples: with_samples,
            resolved_sample_files: self.resolved_samples.len(),
            unresolved_sample_files: self.sample_resolution.len(),
            packed_sample_files: self.packed_samples.len(),
            scripts: self.scripts.iter().filter(|s| !s.is_empty()).count(),
            memory_bytes_total: self.program.params.sample_bytes_total,
        }
    }

    /// The instrument's picture resources, wherever this library keeps them: loose files under
    /// `Resources/pictures/` when the library ships them, otherwise the resource archive
    /// (`.nkr`) that older libraries use instead.
    pub fn picture_assets(&self) -> Vec<crate::archive::PictureAsset> {
        if let Some(assets) = self.loose_picture_assets() {
            return assets;
        }
        match self.resource_archive() {
            Some(archive) => archive.pictures(),
            None => Vec::new(),
        }
    }

    /// Where the picture resources came from, for diagnostics.
    pub fn picture_source(&self) -> Option<PathBuf> {
        if self.loose_picture_assets().is_some() {
            return Some(self.pictures_dir());
        }
        self.resource_archive_path()
    }

    fn pictures_dir(&self) -> PathBuf {
        self.library_root.join("Resources").join("pictures")
    }

    fn loose_picture_assets(&self) -> Option<Vec<crate::archive::PictureAsset>> {
        let dir = self.pictures_dir();
        let entries = std::fs::read_dir(&dir).ok()?;
        let mut assets = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            let extension = path
                .extension()
                .and_then(|extension| extension.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase();
            if !matches!(extension.as_str(), "png" | "jpg" | "jpeg" | "bmp" | "tga") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            let Ok(image) = std::fs::read(&path) else {
                continue;
            };
            let text = std::fs::read_to_string(path.with_extension("txt")).ok();
            assets.push(crate::archive::PictureAsset {
                stem: stem.to_string(),
                image,
                text,
            });
        }
        if assets.is_empty() {
            return None;
        }
        Some(assets)
    }

    /// Where a packed sample lives: the container, the payload start, and its length.
    ///
    /// The catalogue gives the record offset and the payload length. A sample record is preceded
    /// by a 31-byte `0x16CCF80A` block header, so the `.ncw` itself starts at `offset + 31` — the
    /// entry's `size` counts the payload only, which is why consecutive entries are spaced
    /// `size + 31` apart.
    pub fn packed_sample_record(&self, id: i32) -> Option<(PathBuf, usize, usize)> {
        let entry = self.packed_samples.get(&id)?;
        let size = entry.size? as usize;
        let start = entry.offset as usize + SAMPLE_RECORD_HEADER;
        let container = find_sample_record(&self.sample_containers(), start, size)?;
        Some((container, start, size))
    }

    /// Containers that could hold packed samples: `.nkx` first, then `.nkr`.
    fn sample_containers(&self) -> Vec<PathBuf> {
        let mut candidates: Vec<PathBuf> = Vec::new();
        for directory in [
            Some(self.library_root.clone()),
            Some(self.library_root.join("Resources")),
            self.source_path.parent().map(Path::to_path_buf),
        ]
        .into_iter()
        .flatten()
        {
            let Ok(entries) = std::fs::read_dir(&directory) else {
                continue;
            };
            let mut found: Vec<(u8, PathBuf)> = entries
                .flatten()
                .filter_map(|entry| {
                    let path = entry.path();
                    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
                    let rank = match extension.as_str() {
                        "nkx" => 0,
                        "nkr" => 1,
                        _ => return None,
                    };
                    Some((rank, path))
                })
                .collect();
            found.sort();
            candidates.extend(found.into_iter().map(|(_, path)| path));
        }
        candidates.dedup();
        candidates
    }

    /// Find the resource archive that belongs to this library, if there is one.
    /// Find the library's resource archive.
    ///
    /// There is no single convention: some libraries keep `<instrument>.nkr` next to the `.nki`,
    /// others put everything under `Resources/` (`Resources.nkr`) with the instruments in a
    /// sibling folder. The archive is looked for in the library root, then one level down, then
    /// beside the `.nki` itself.
    pub fn resource_archive_path(&self) -> Option<PathBuf> {
        let is_archive = |path: &Path| {
            path.extension()
                .and_then(|extension| extension.to_str())
                .map(|extension| extension.eq_ignore_ascii_case("nkr"))
                .unwrap_or(false)
        };
        let mut candidates: Vec<PathBuf> = Vec::new();
        for directory in [
            Some(self.library_root.clone()),
            Some(self.library_root.join("Resources")),
            self.source_path.parent().map(Path::to_path_buf),
        ]
        .into_iter()
        .flatten()
        {
            let Ok(entries) = std::fs::read_dir(&directory) else {
                continue;
            };
            candidates.extend(
                entries
                    .flatten()
                    .map(|entry| entry.path())
                    .filter(|path| is_archive(path)),
            );
        }
        // Prefer an archive that is populated over one that is not, then the first by name.
        candidates.sort();
        candidates.into_iter().next()
    }

    /// Find the library's `.nkc` catalogue, following the same conventions as the archive.
    pub fn nkc_index_path(&self) -> Option<PathBuf> {
        let is_index = |path: &Path| {
            path.extension()
                .and_then(|extension| extension.to_str())
                .map(|extension| extension.eq_ignore_ascii_case("nkc"))
                .unwrap_or(false)
        };
        let mut candidates: Vec<PathBuf> = Vec::new();
        for directory in [
            Some(self.library_root.clone()),
            Some(self.library_root.join("Resources")),
            self.source_path.parent().map(Path::to_path_buf),
        ]
        .into_iter()
        .flatten()
        {
            let Ok(entries) = std::fs::read_dir(&directory) else {
                continue;
            };
            candidates.extend(
                entries
                    .flatten()
                    .map(|entry| entry.path())
                    .filter(|path| is_index(path)),
            );
        }
        candidates.sort();
        candidates.into_iter().next()
    }

    /// Parse the library's `.nkc` catalogue, when it ships one.
    ///
    /// A packed library addresses its samples by name and offset through this index; the payload
    /// itself lives in `.nkx`/`.nkr` and, for `.ncw` samples, still needs a codec we do not have.
    pub fn nkc_index(&self) -> Option<crate::nkc::NkcIndex> {
        let path = self.nkc_index_path()?;
        let bytes = std::fs::read(&path).ok()?;
        crate::nkc::parse_nkc(&bytes).ok()
    }

    pub fn resource_archive(&self) -> Option<crate::archive::ResourceArchive> {
        let path = self.resource_archive_path()?;
        let bytes = std::fs::read(&path).ok()?;
        // The catalogue says exactly where each payload starts. The order scan stays as a
        // fallback for a catalogue whose offsets do not describe this container at all — it caught
        // a real case while the record layout was being read name-first, and costs one comparison
        // to keep.
        let from_catalogue = self
            .nkc_index()
            .and_then(|index| crate::archive::archive_from_index(&bytes, &index));
        let scanned = crate::archive::parse_resource_archive(&bytes);
        choose_archive(from_catalogue, scanned)
    }
}

/// Parse `<library>/Resources/*.nkc`, `<library>/*.nkc` or the one beside the `.nki`.
fn parse_nkc_index(source: &Path, library_root: &Path) -> Option<crate::nkc::NkcIndex> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    for directory in [
        Some(library_root.to_path_buf()),
        Some(library_root.join("Resources")),
        source.parent().map(Path::to_path_buf),
    ]
    .into_iter()
    .flatten()
    {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        candidates.extend(entries.flatten().map(|entry| entry.path()).filter(|path| {
            path.extension()
                .and_then(|extension| extension.to_str())
                .map(|extension| extension.eq_ignore_ascii_case("nkc"))
                .unwrap_or(false)
        }));
    }
    candidates.sort();
    for path in candidates {
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        if let Ok(index) = crate::nkc::parse_nkc(&bytes) {
            return Some(index);
        }
    }
    None
}

/// Union of the zone key ranges, dropped to the Program range and to MIDI 0..=127.
fn mapped_key_mask(
    ranges: impl Iterator<Item = (i16, i16)>,
    program_low: u8,
    program_high: u8,
) -> [bool; 128] {
    let mut keys = [false; 128];
    for (low, high) in ranges {
        if high < 0 || low > 127 {
            continue;
        }
        for key in low.max(0)..=high.min(127) {
            keys[key as usize] = true;
        }
    }
    for (note, mapped) in keys.iter_mut().enumerate() {
        if note < program_low as usize || note > program_high as usize {
            *mapped = false;
        }
    }
    keys
}

/// Every packed sample record is preceded by this many bytes of block header.
const SAMPLE_RECORD_HEADER: usize = 31;

fn looks_like_image(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".png") || lower.ends_with(".jpg") || lower.ends_with(".jpeg")
}

#[derive(Clone, Debug)]
pub struct InstrumentStats {
    pub groups: usize,
    pub zones: usize,
    pub zones_with_samples: usize,
    pub resolved_sample_files: usize,
    pub unresolved_sample_files: usize,
    pub packed_sample_files: usize,
    pub scripts: usize,
    pub memory_bytes_total: f64,
}

#[derive(Default)]
pub struct LoadedSamples {
    pub samples: HashMap<i32, Arc<AudioSample>>,
    pub diagnostics: Vec<Diagnostic>,
    /// Bytes actually held in memory (DFD preload of streamed files, all of decoded files).
    pub bytes_loaded: u64,
    /// Bytes mapped from disk but not touched.
    pub bytes_mapped: u64,
}

/// Load every resolved sample used by the instrument. Missing files are diagnostics, not errors:
/// the instrument still loads and the affected zones remain silent.
pub fn load_samples(instrument: &Instrument) -> LoadedSamples {
    let mut loaded = LoadedSamples::default();
    let mut counted_maps: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    // Two filename ids can name the same file; mapping it twice would double the memory for
    // exactly the same audio. Share the decoded sample instead.
    let mut by_source: HashMap<PathBuf, Arc<AudioSample>> = HashMap::new();
    for id in instrument.zone_sample_ids() {
        // Stream the file: mapping keeps 10 GB libraries off the heap, and the instrument's DFD
        // preload setting decides how much is touched up front.
        // `dfd_preload` is Kontakt's preload buffer size in *bytes* (60 KiB by default, 240 KiB on
        // 8Dio's 1928 Piano), not a frame count.
        let preload = instrument.program.params.dfd_preload.max(0) as usize;
        let loose = instrument.resolved_samples.get(&id);
        // A packed library has no loose file: the audio is a `.ncw` inside an `.nkx`/`.nkr` and
        // the catalogue says where.
        let packed = if loose.is_none() {
            instrument.packed_sample_record(id)
        } else {
            None
        };
        let label = loose.cloned().unwrap_or_else(|| {
            instrument
                .packed_samples
                .get(&id)
                .map(|entry| PathBuf::from(entry.name.clone()))
                .unwrap_or_default()
        });
        let source_key = loose.cloned().or_else(|| {
            packed.as_ref().map(|(container, start, _)| {
                PathBuf::from(format!("{}#{}", container.display(), start))
            })
        });
        if let Some(key) = &source_key
            && let Some(shared) = by_source.get(key)
        {
            loaded.samples.insert(id, Arc::clone(shared));
            continue;
        }
        let path = &label;
        let opened = match (&loose, &packed) {
            (Some(path), _) => open_sample(path, preload),
            (None, Some((container, start, length))) => {
                crate::wav::map_ncw_range(path, container, *start, *length, preload)
            }
            (None, None) => {
                if instrument.packed_samples.contains_key(&id) {
                    loaded.diagnostics.push(Diagnostic {
                        severity: DiagnosticSeverity::Warning,
                        area: "samples",
                        message: format!(
                            "{}: packed in the catalogue but no container holds it at that offset",
                            label.display()
                        ),
                    });
                }
                continue;
            }
        };
        match opened {
            Ok(sample) => {
                // Cross-check the stored zone hint against the file header. The first zone using
                // this sample is enough; all zones for one file should agree.
                if let Some(zone) = instrument
                    .zones
                    .iter()
                    .find(|z| z.sample.as_ref().is_some_and(|s| s.filename_id == id))
                {
                    if let Some(info) = &zone.sample {
                        if info.sample_rate > 0 && info.sample_rate != sample.sample_rate as i32 {
                            loaded.diagnostics.push(Diagnostic {
                                severity: DiagnosticSeverity::Warning,
                                area: "samples",
                                message: format!(
                                    "{}: zone stores {} Hz but the file is {} Hz; using the file header",
                                    path.display(),
                                    info.sample_rate,
                                    sample.sample_rate
                                ),
                            });
                        }
                        if info.channels > 0 && info.channels != sample.channels as u8 {
                            loaded.diagnostics.push(Diagnostic {
                                severity: DiagnosticSeverity::Warning,
                                area: "samples",
                                message: format!(
                                    "{}: zone stores {} channel(s) but the file has {}; using the file header",
                                    path.display(),
                                    info.channels,
                                    sample.channels
                                ),
                            });
                        }
                        if info.frames > 0 && info.frames != sample.frames as i32 {
                            loaded.diagnostics.push(Diagnostic {
                                severity: DiagnosticSeverity::Warning,
                                area: "samples",
                                message: format!(
                                    "{}: zone stores {} frames but the file has {}; using the file header",
                                    path.display(),
                                    info.frames,
                                    sample.frames
                                ),
                            });
                        }
                    }
                }
                if sample.is_streaming() {
                    // Packed samples share one container mapping; count each file once.
                    if sample
                        .mapped_source()
                        .is_some_and(|source| counted_maps.insert(source.to_path_buf()))
                    {
                        loaded.bytes_mapped += sample.mapped_bytes();
                    }
                    loaded.bytes_loaded += sample.resident_bytes;
                } else {
                    loaded.bytes_loaded += sample.resident_bytes;
                }
                let sample = Arc::new(sample);
                if let Some(key) = source_key {
                    by_source.insert(key, Arc::clone(&sample));
                }
                loaded.samples.insert(id, sample);
            }
            Err(error) => loaded.diagnostics.push(Diagnostic {
                severity: DiagnosticSeverity::Warning,
                area: "samples",
                message: error.to_string(),
            }),
        }
    }
    loaded
}

/// Helper used by tests and the inspector binary.
pub fn parse_without_samples(path: impl AsRef<Path>) -> Result<Instrument> {
    Instrument::load(path)
}

#[allow(dead_code)]
fn read_u32(reader: &mut Reader<'_>) -> Result<u32> {
    reader.u32()
}

/// First container that holds an NCW record at `start` with a payload of `size` bytes.
fn find_sample_record(containers: &[PathBuf], start: usize, size: usize) -> Option<PathBuf> {
    use std::io::{Read, Seek, SeekFrom};
    for container in containers {
        let Ok(mut file) = std::fs::File::open(container) else {
            continue;
        };
        let Ok(length) = file.metadata().map(|meta| meta.len()) else {
            continue;
        };
        // A record that runs past the end of the container is not this sample, and clamping it
        // would decode a truncated file as if it were whole.
        if (start + size) as u64 > length || file.seek(SeekFrom::Start(start as u64)).is_err() {
            continue;
        }
        let mut magic = [0u8; 4];
        if file.read_exact(&mut magic).is_err() {
            continue;
        }
        if magic == crate::ncw::FILE_MAGIC {
            return Some(container.clone());
        }
    }
    None
}

/// Pick between the catalogue's slicing and the order scan.
///
/// The catalogue is the better answer whenever it produces pictures, because it addresses payloads
/// by offset instead of assuming the directory order matches the file. When it produces none — the
/// offsets describe something other than the sprite payloads — the scan is the only route that can
/// recover them, so it wins. No library on hand needs this any more, but a catalogue that describes
/// a different container would produce exactly that shape.
fn choose_archive(
    from_catalogue: Option<crate::archive::ResourceArchive>,
    scanned: crate::archive::ResourceArchive,
) -> Option<crate::archive::ResourceArchive> {
    if let Some(archive) = &from_catalogue
        && !archive.pictures().is_empty()
    {
        return Some(archive.clone());
    }
    if !scanned.pictures().is_empty() {
        return Some(scanned);
    }
    if from_catalogue.is_some() {
        return from_catalogue;
    }
    if scanned.entries.is_empty() {
        return None;
    }
    Some(scanned)
}

/// Follow `relative` under `root`, matching each component case-insensitively.
///
/// Factory references are stored lower case (`presets/…`) while the installed folders are not
/// (`Presets/…`), which matters on a case-sensitive file system.
fn resolve_case_insensitive(root: &Path, relative: &Path) -> Option<PathBuf> {
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let std::path::Component::Normal(name) = component else {
            continue;
        };
        let candidate = current.join(name);
        if candidate.exists() {
            current = candidate;
            continue;
        }
        let wanted = name.to_string_lossy().to_ascii_lowercase();
        let mut matched = None;
        for entry in std::fs::read_dir(&current).ok()?.flatten() {
            if entry.file_name().to_string_lossy().to_ascii_lowercase() == wanted {
                matched = Some(entry.path());
                break;
            }
        }
        current = matched?;
    }
    Some(current)
}

/// Split a `<container>.nkr/<path inside>` reference into its two halves.
///
/// Packed libraries address a resource this way: the package is a real file, and what follows its
/// extension is a path *inside* it that only the `.nkc` catalogue can resolve.
fn split_packed_reference(reference: &str) -> Option<(PathBuf, String)> {
    let components: Vec<&str> = reference.split(['/', '\\']).collect();
    let package = components.iter().position(|component| {
        let lower = component.to_ascii_lowercase();
        lower.ends_with(".nkr") || lower.ends_with(".nkx")
    })?;
    let container = components[..=package].join("/");
    let inner = components[package + 1..].join("/");
    if container.is_empty() || inner.is_empty() {
        return None;
    }
    Some((PathBuf::from(container), inner))
}

/// Resolve `.` and `..` without touching the filesystem, so a reference that climbs out of the
/// instrument's folder still names the file it means.
fn normalise_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{find_sample_record, mapped_key_mask, normalise_path, split_packed_reference};
    use std::path::{Path, PathBuf};

    /// A sample record is a 31-byte block header followed by the `.ncw`; the catalogue offset
    /// points at the header and its `size` covers the payload only.
    #[test]
    fn finds_a_sample_record_past_the_block_header() {
        let path =
            std::env::temp_dir().join(format!("openkontakt-container-{}.nkr", std::process::id()));
        let mut container = vec![0xA5u8; 31];
        container.extend_from_slice(&crate::ncw::FILE_MAGIC);
        container.extend_from_slice(&[7u8; 64]);
        std::fs::write(&path, &container).unwrap();

        let containers = vec![path.clone()];
        assert_eq!(
            find_sample_record(&containers, 31, 68),
            Some(path.clone()),
            "the payload starts past the 31-byte header"
        );
        // Reading from the record start instead of past the header finds filler, not a signature.
        assert!(find_sample_record(&containers, 0, 68).is_none());
        // A record that runs past the end of the container is rejected, not clamped.
        assert!(find_sample_record(&containers, 31, 9_999).is_none());
        let _ = std::fs::remove_file(&path);
    }

    /// 8Dio writes its impulse responses as `<package>.nkr/<path inside the package>`.
    fn archive_with(name: &str, data: &[u8]) -> crate::archive::ResourceArchive {
        crate::archive::ResourceArchive {
            entries: vec![crate::archive::ArchiveEntry {
                name: name.to_string(),
                data: data.to_vec(),
            }],
        }
    }

    /// A catalogue whose offsets miss the sprite payloads must not hide them.
    #[test]
    fn an_image_less_catalogue_result_steps_aside_for_the_scan() {
        let png = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        let catalogue = archive_with("1_Art_1.PNG", b"Has Alpha Channel: yes");
        let scanned = archive_with("1_Art_1.PNG", &png);
        let chosen = super::choose_archive(Some(catalogue), scanned).expect("an archive");
        assert_eq!(chosen.pictures().len(), 1, "the scan has the real sprite");

        // When the catalogue does have pictures it is preferred.
        let catalogue = archive_with("1_Art_1.PNG", &png);
        let scanned = archive_with("other.PNG", &png);
        let chosen = super::choose_archive(Some(catalogue), scanned).expect("an archive");
        assert_eq!(chosen.entries[0].name, "1_Art_1.PNG");

        // No pictures anywhere: keep the catalogue result, which may still carry other payloads.
        let catalogue = archive_with("notes.txt", b"hello");
        let scanned = crate::archive::ResourceArchive::default();
        let chosen = super::choose_archive(Some(catalogue), scanned).expect("an archive");
        assert_eq!(chosen.entries[0].name, "notes.txt");

        assert!(super::choose_archive(None, crate::archive::ResourceArchive::default()).is_none());
    }

    #[test]
    fn splits_a_reference_that_points_into_a_package() {
        let (container, inner) = split_packed_reference(
            "../Resources/Resources.nkr/Resources/ir_samples/01_Transform.NCW",
        )
        .expect("packed reference");
        assert_eq!(container, PathBuf::from("../Resources/Resources.nkr"));
        assert_eq!(inner, "Resources/ir_samples/01_Transform.NCW");
        assert!(split_packed_reference("../Resources/Wallpaper.png").is_none());
        assert!(split_packed_reference("Resources.nkr").is_none());
    }

    #[test]
    fn normalises_a_reference_that_climbs_out_of_a_folder() {
        let path = Path::new("/library/Instruments").join("../Resources/Resources.nkr");
        assert_eq!(
            normalise_path(&path),
            PathBuf::from("/library/Resources/Resources.nkr")
        );
    }

    #[test]
    fn mapped_keys_are_the_union_of_zone_ranges() {
        let mask = mapped_key_mask([(36, 48), (60, 72)].into_iter(), 0, 127);
        assert!(mask[36] && mask[48] && mask[60] && mask[72]);
        assert!(!mask[35] && !mask[49] && !mask[59] && !mask[73]);
        assert_eq!(mask.iter().filter(|mapped| **mapped).count(), 13 + 13);
    }

    #[test]
    fn mapped_keys_respect_the_program_range() {
        let mask = mapped_key_mask([(36, 48)].into_iter(), 40, 44);
        assert!(!mask[39] && mask[40] && mask[44] && !mask[45]);
    }

    #[test]
    fn mapped_keys_ignore_ranges_outside_midi() {
        let mask = mapped_key_mask([(-5, 3), (125, 140)].into_iter(), 0, 127);
        assert!(mask[0] && mask[3] && mask[125] && mask[127]);
        assert_eq!(mask.iter().filter(|mapped| **mapped).count(), 4 + 3);
    }
}
