//! Sample path resolution per `docs/08-samples-and-resolution.md` §08.6.
//!
//! Resolution is deliberately conservative: exact base-relative matches first, then a lazily built
//! case-insensitive basename index. Ambiguity is reported instead of guessed.

use crate::filename::{FileName, FileNameTable};
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolutionDiagnostic {
    pub kind: ResolutionDiagnosticKind,
    pub message: String,
    pub requested: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResolutionDiagnosticKind {
    Missing,
    Ambiguous,
    ArchiveMember,
    OutsideRoot,
}

#[derive(Clone, Debug, Default)]
pub struct ResolvedSample {
    pub path: Option<PathBuf>,
    pub requested: Option<String>,
    pub diagnostic: Option<ResolutionDiagnostic>,
}

#[derive(Debug)]
pub struct SampleResolver {
    pub instrument_dir: PathBuf,
    pub library_root: PathBuf,
    basename_index: Option<HashMap<String, Vec<PathBuf>>>,
}

impl SampleResolver {
    pub fn new(instrument_path: &Path) -> Self {
        let instrument_dir = instrument_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        let library_root = find_library_root(&instrument_dir);
        Self {
            instrument_dir,
            library_root,
            basename_index: None,
        }
    }

    pub fn resolve(&mut self, table: &FileNameTable, filename_id: i32) -> ResolvedSample {
        let Some(entry) = table.sample(filename_id) else {
            return ResolvedSample {
                path: None,
                requested: None,
                diagnostic: Some(ResolutionDiagnostic {
                    kind: ResolutionDiagnosticKind::Missing,
                    message: format!("filename table has no entry {filename_id}"),
                    requested: None,
                }),
            };
        };
        self.resolve_entry(entry)
    }

    /// Resolve any FileName record (sample, wallpaper, other resource) with the same root and
    /// basename rules. The caller decides which table/list supplied the record.
    pub fn resolve_entry(&mut self, entry: &FileName) -> ResolvedSample {
        let requested = entry.display_path();
        if requested.is_empty() {
            return ResolvedSample {
                path: None,
                requested: Some(requested),
                diagnostic: Some(ResolutionDiagnostic {
                    kind: ResolutionDiagnosticKind::Missing,
                    message: "filename entry is empty".into(),
                    requested: None,
                }),
            };
        }
        if is_archive_member(&requested) {
            return ResolvedSample {
                path: None,
                requested: Some(requested.clone()),
                diagnostic: Some(ResolutionDiagnostic {
                    kind: ResolutionDiagnosticKind::ArchiveMember,
                    message: format!(
                        "{requested} points inside an NKX/NKR archive; archive containers are not supported yet"
                    ),
                    requested: Some(requested),
                }),
            };
        }

        let normalized = normalize_slashes(&requested);
        let mut variants = vec![normalized.clone()];
        if let Some(trimmed) = trim_library_folder(&normalized, &self.instrument_dir) {
            variants.push(trimmed);
        }

        let bases = self.search_bases();
        for variant in &variants {
            for base in &bases {
                let candidate = join_lexical(base, variant);
                let candidate = normalize_path(&candidate);
                if !is_within(&candidate, &self.library_root) {
                    continue;
                }
                if candidate.is_file() && canonical_within(&candidate, &self.library_root) {
                    return ResolvedSample {
                        path: Some(candidate),
                        requested: Some(requested),
                        diagnostic: None,
                    };
                }
            }
        }

        // Factory resources (an instrument's impulse response, for one) are referenced through a
        // root segment and live in the installed Kontakt's shared content, not in the library
        // folder. Only entries that carry such an anchor are looked up there, so a library sample
        // can never be "resolved" into factory content by accident.
        if entry
            .segments
            .first()
            .is_some_and(|segment| segment.kind == 6 || segment.kind == 11)
        {
            let relative = factory_relative(&normalized);
            for root in factory_roots() {
                let candidate = normalize_path(&root.join(&relative));
                if candidate.is_file() {
                    return ResolvedSample {
                        path: Some(candidate),
                        requested: Some(requested),
                        diagnostic: None,
                    };
                }
            }
        }

        if self.basename_index.is_none() {
            let mut index = HashMap::new();
            index_files(&self.library_root, &self.library_root, 0, &mut index);
            self.basename_index = Some(index);
        }
        if let Some(index) = self.basename_index.as_ref() {
            let basename = Path::new(&normalized)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase();
            let matches = index.get(&basename).cloned().unwrap_or_default();
            if matches.len() == 1 {
                return ResolvedSample {
                    path: Some(matches[0].clone()),
                    requested: Some(requested),
                    diagnostic: None,
                };
            }
            if matches.len() > 1 {
                if let Some(found) = disambiguate_by_suffix(&matches, &normalized) {
                    return ResolvedSample {
                        path: Some(found),
                        requested: Some(requested),
                        diagnostic: None,
                    };
                }
                return ResolvedSample {
                    path: None,
                    requested: Some(requested.clone()),
                    diagnostic: Some(ResolutionDiagnostic {
                        kind: ResolutionDiagnosticKind::Ambiguous,
                        message: format!(
                            "{requested}: {} files share this basename and no two-component suffix disambiguates them",
                            matches.len()
                        ),
                        requested: Some(requested),
                    }),
                };
            }
        }

        ResolvedSample {
            path: None,
            requested: Some(requested.clone()),
            diagnostic: Some(ResolutionDiagnostic {
                kind: ResolutionDiagnosticKind::Missing,
                message: format!(
                    "{requested} was not found under {}",
                    self.library_root.display()
                ),
                requested: Some(requested),
            }),
        }
    }

    fn search_bases(&self) -> Vec<PathBuf> {
        let mut bases = Vec::new();
        let mut current = Some(self.instrument_dir.as_path());
        let mut depth = 0;
        while let Some(path) = current {
            bases.push(path.to_path_buf());
            if path == self.library_root || depth >= 8 {
                break;
            }
            current = path.parent();
            depth += 1;
        }
        if !bases.iter().any(|path| path == &self.library_root) {
            bases.push(self.library_root.clone());
        }
        bases
    }
}

/// Walk ancestors nearest-first, accepting the first one containing `Samples`, `Resources`, or a
/// `.nicnt` file. Falls back to the instrument folder.
pub fn find_library_root(instrument_dir: &Path) -> PathBuf {
    let mut current = Some(instrument_dir);
    let mut depth = 0;
    while let Some(path) = current {
        if path.join("Samples").is_dir() || path.join("Resources").is_dir() || contains_nicnt(path)
        {
            return path.to_path_buf();
        }
        if depth >= 8 {
            break;
        }
        current = path.parent();
        depth += 1;
    }
    instrument_dir.to_path_buf()
}

fn contains_nicnt(path: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(path) else {
        return false;
    };
    entries.flatten().any(|entry| {
        entry
            .path()
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("nicnt"))
    })
}

/// The part of a factory path below the content root: `/presets/Effects/...` becomes
/// `presets/Effects/...`.
fn factory_relative(path: &str) -> PathBuf {
    let trimmed = path.trim_start_matches('/');
    // The anchor may also be a drive letter (`C:`) on a Windows-authored library.
    match trimmed.split_once(':') {
        Some((head, rest)) if head.len() == 1 => PathBuf::from(rest.trim_start_matches('/')),
        _ => PathBuf::from(trimmed),
    }
}

/// The installed Kontakt's shared content root, newest version first.
///
/// Factory resources (impulse responses, for one) live here rather than in the library folder, and
/// scripts reach them through `_get_folder($GET_FOLDER_FACTORY_DIR)`.
pub fn factory_content_root() -> Option<PathBuf> {
    factory_roots().into_iter().next()
}

/// Kontakt's shared content roots, newest version first.
fn factory_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    for base in [
        PathBuf::from("/Library/Application Support/Native Instruments"),
        PathBuf::from(std::env::var("HOME").unwrap_or_default())
            .join("Library/Application Support/Native Instruments"),
    ] {
        let Ok(entries) = std::fs::read_dir(&base) else {
            continue;
        };
        let mut found: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .map(|name| name.to_ascii_lowercase().starts_with("kontakt"))
                    .unwrap_or(false)
            })
            .collect();
        found.sort();
        found.reverse();
        roots.extend(found);
    }
    roots
}

fn is_archive_member(path: &str) -> bool {
    path.split(['/', '\\', '|']).any(|part| {
        let lower = part.to_ascii_lowercase();
        lower.ends_with(".nkx") || lower.ends_with(".nkr") || lower.ends_with(".nkc")
    })
}

fn normalize_slashes(path: &str) -> String {
    path.replace(['\\', '|'], "/")
}

fn trim_library_folder(path: &str, instrument_dir: &Path) -> Option<String> {
    let folder = instrument_dir.file_name()?.to_str()?;
    let trimmed = path.strip_prefix(folder)?.strip_prefix('/')?;
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn join_lexical(base: &Path, relative: &str) -> PathBuf {
    let relative = relative.trim_start_matches('/');
    let mut path = base.to_path_buf();
    for part in relative.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            path.pop();
        } else if part.len() == 2 && part.ends_with(':') {
            // A drive anchor such as C: is meaningless on a library mount; drop it.
            continue;
        } else {
            path.push(part);
        }
    }
    path
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn is_within(path: &Path, root: &Path) -> bool {
    let path = normalize_path(path);
    let root = normalize_path(root);
    path == root || path.starts_with(&root)
}

fn canonical_within(path: &Path, root: &Path) -> bool {
    let Ok(canonical_path) = path.canonicalize() else {
        return false;
    };
    let Ok(canonical_root) = root.canonicalize() else {
        return false;
    };
    canonical_path.starts_with(canonical_root)
}

fn index_files(
    root: &Path,
    directory: &Path,
    depth: usize,
    index: &mut HashMap<String, Vec<PathBuf>>,
) {
    if depth > 16 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if file_type.is_dir() {
            index_files(root, &path, depth + 1, index);
        } else if file_type.is_file() {
            if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
                index
                    .entry(name.to_ascii_lowercase())
                    .or_default()
                    .push(path);
            }
        }
    }
    let _ = root;
}

fn disambiguate_by_suffix(matches: &[PathBuf], requested: &str) -> Option<PathBuf> {
    let requested_components: Vec<String> = requested
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".." && *part != ".")
        .map(|part| part.to_ascii_lowercase())
        .collect();
    if requested_components.len() < 2 {
        return None;
    }
    for length in 2..=requested_components.len() {
        let suffix = &requested_components[requested_components.len() - length..];
        let mut found = None;
        let mut count = 0;
        for candidate in matches {
            let candidate_components: Vec<String> = candidate
                .components()
                .filter_map(|component| component.as_os_str().to_str())
                .map(|part| part.to_ascii_lowercase())
                .collect();
            if candidate_components.ends_with(suffix) {
                count += 1;
                found = Some(candidate.clone());
            }
        }
        if count == 1 {
            return found;
        }
    }
    None
}
