use crate::model::FileEntry;
use std::time::UNIX_EPOCH;
use walkdir::{DirEntry, WalkDir};

fn is_hidden(entry: &DirEntry) -> bool {
    entry
        .file_name()
        .to_str()
        .map(|s| s.starts_with('.'))
        .unwrap_or(false)
}

/// Recursively walks the given folders, collecting file metadata.
/// Calls `on_progress(folder, files_found_so_far)` periodically.
pub fn walk_folders<F: FnMut(&str, u64)>(
    folders: &[String],
    include_hidden: bool,
    min_file_size: u64,
    mut on_progress: F,
) -> Vec<FileEntry> {
    let mut entries = Vec::new();

    for folder in folders {
        let walker = WalkDir::new(folder)
            .into_iter()
            .filter_entry(|e| include_hidden || e.depth() == 0 || !is_hidden(e));

        for item in walker {
            let Ok(item) = item else { continue };
            if !item.file_type().is_file() {
                continue;
            }
            let Ok(meta) = item.metadata() else { continue };
            let size = meta.len();
            if size < min_file_size {
                continue;
            }
            let modified = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64);

            entries.push(FileEntry {
                path: item.path().to_string_lossy().to_string(),
                size,
                modified,
            });

            if entries.len() % 100 == 0 {
                on_progress(folder, entries.len() as u64);
            }
        }
        on_progress(folder, entries.len() as u64);
    }

    entries
}
