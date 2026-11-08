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
use crate::wav::{AudioSample, map_wav};
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

    pub fn resource_archive(&self) -> Option<crate::archive::ResourceArchive> {
        let path = self.resource_archive_path()?;
        let bytes = std::fs::read(&path).ok()?;
        let archive = crate::archive::parse_resource_archive(&bytes);
        if archive.entries.is_empty() {
            return None;
        }
        Some(archive)
    }
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
    for id in instrument.zone_sample_ids() {
        let Some(path) = instrument.resolved_samples.get(&id) else {
            continue;
        };
        // Stream the file: mapping keeps 10 GB libraries off the heap, and the instrument's DFD
        // preload setting decides how much is touched up front.
        let preload = instrument.program.params.dfd_preload.max(0) as usize;
        match map_wav(path, preload) {
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
                                    "{}: zone stores {} Hz but WAV is {} Hz; using the WAV header",
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
                                    "{}: zone stores {} channel(s) but WAV has {}; using the WAV header",
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
                                    "{}: zone stores {} frames but WAV has {}; using the WAV header",
                                    path.display(),
                                    info.frames,
                                    sample.frames
                                ),
                            });
                        }
                    }
                }
                if sample.is_streaming() {
                    loaded.bytes_mapped += sample.mapped_bytes();
                    loaded.bytes_loaded += sample.resident_bytes;
                } else {
                    loaded.bytes_loaded += sample.resident_bytes;
                }
                loaded.samples.insert(id, Arc::new(sample));
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

#[cfg(test)]
mod tests {
    use super::mapped_key_mask;

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
