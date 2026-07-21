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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn names_of(entries: &[FileEntry]) -> Vec<String> {
        let mut names: Vec<String> = entries
            .iter()
            .map(|e| {
                std::path::Path::new(&e.path)
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        names
    }

    fn setup(name: &str) -> std::path::PathBuf {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(name);
        if root.exists() {
            fs::remove_dir_all(&root).unwrap();
        }
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn filters_files_below_min_size() {
        let root = setup("scanner-test-min-size");
        fs::write(root.join("small.bin"), vec![0u8; 10]).unwrap();
        fs::write(root.join("big.bin"), vec![0u8; 100]).unwrap();

        let folder = root.to_string_lossy().to_string();
        let entries = walk_folders(&[folder], true, 50, |_, _| {});

        assert_eq!(names_of(&entries), vec!["big.bin"]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn hidden_files_and_folders_are_excluded_by_default() {
        let root = setup("scanner-test-hidden");
        fs::write(root.join("visible.bin"), vec![0u8; 10]).unwrap();
        fs::write(root.join(".hidden.bin"), vec![0u8; 10]).unwrap();
        fs::create_dir(root.join(".hidden-dir")).unwrap();
        fs::write(root.join(".hidden-dir").join("inner.bin"), vec![0u8; 10]).unwrap();

        let folder = root.to_string_lossy().to_string();

        let visible_only = walk_folders(std::slice::from_ref(&folder), false, 0, |_, _| {});
        assert_eq!(names_of(&visible_only), vec!["visible.bin"]);

        let with_hidden = walk_folders(&[folder], true, 0, |_, _| {});
        assert_eq!(
            names_of(&with_hidden),
            vec![".hidden.bin", "inner.bin", "visible.bin"]
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_dotted_top_level_scan_root_is_still_walked() {
        let root = setup(".scanner-test-dotted-root");
        fs::write(root.join("file.bin"), vec![0u8; 10]).unwrap();

        let folder = root.to_string_lossy().to_string();
        let entries = walk_folders(&[folder], false, 0, |_, _| {});

        assert_eq!(names_of(&entries), vec!["file.bin"]);
        fs::remove_dir_all(root).unwrap();
    }
}
