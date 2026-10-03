use crate::{
    model::FileEntry,
    scan_control::{FileStamp, ScanControl},
};
use std::collections::HashSet;
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
#[cfg(test)]
pub fn walk_folders<F: FnMut(&str, u64)>(
    folders: &[String],
    include_hidden: bool,
    min_file_size: u64,
    on_progress: F,
) -> Vec<FileEntry> {
    walk_folders_controlled(
        folders,
        include_hidden,
        min_file_size,
        on_progress,
        &ScanControl::default(),
    )
    .unwrap()
}

pub fn walk_folders_controlled<F: FnMut(&str, u64)>(
    folders: &[String],
    include_hidden: bool,
    min_file_size: u64,
    mut on_progress: F,
    control: &ScanControl,
) -> std::io::Result<Vec<FileEntry>> {
    let mut entries = Vec::new();
    let mut seen = HashSet::new();
    for folder in folders {
        control.check()?;
        let walker = WalkDir::new(folder)
            .into_iter()
            .filter_entry(|e| include_hidden || e.depth() == 0 || !is_hidden(e));
        for item in walker {
            control.check()?;
            let item = match item {
                Ok(item) => item,
                Err(error) => {
                    control.warn(folder, error);
                    continue;
                }
            };
            if !item.file_type().is_file() {
                continue;
            }
            let result = (|| -> std::io::Result<_> {
                let path = item.path().canonicalize()?;
                let path = path.into_os_string().into_string().map_err(|_| {
                    std::io::Error::other("Filename cannot be represented as UTF-8")
                })?;
                let meta = std::fs::symlink_metadata(&path)?;
                let stamp = FileStamp::from_metadata(&meta)?;
                Ok((path, meta, stamp))
            })();
            let (path, meta, stamp) = match result {
                Ok(value) => value,
                Err(error) => {
                    control.warn(&item.path().display().to_string(), error);
                    continue;
                }
            };
            if !seen.insert(path.clone()) || meta.len() < min_file_size {
                continue;
            }
            let modified = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64);
            control.stamps.lock().unwrap().insert(path.clone(), stamp);
            entries.push(FileEntry {
                path,
                size: meta.len(),
                modified,
            });
            if entries.len() % 100 == 0 {
                on_progress(folder, entries.len() as u64);
            }
        }
        on_progress(folder, entries.len() as u64);
    }
    control.check()?;
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[cfg(unix)]
    #[test]
    fn permission_denied_is_reported_without_losing_readable_files() {
        use std::os::unix::fs::PermissionsExt;
        let dir = crate::test_support::TestDir::new();
        let blocked = dir.0.join("blocked");
        fs::create_dir(&blocked).unwrap();
        dir.file("visible", b"visible");
        dir.file("blocked/secret", b"secret");
        fs::set_permissions(&blocked, fs::Permissions::from_mode(0o000)).unwrap();
        let control = ScanControl::default();
        let entries = walk_folders_controlled(
            &[dir.0.to_str().unwrap().into()],
            true,
            0,
            |_, _| {},
            &control,
        )
        .unwrap();
        fs::set_permissions(&blocked, fs::Permissions::from_mode(0o700)).unwrap();
        // Elevated test runners can bypass POSIX permissions; normal desktop
        // users must get an explicit issue and keep the readable results.
        if entries.len() == 1 {
            assert!(!control.warnings().is_empty());
            assert!(entries[0].path.ends_with("visible"));
        } else {
            assert_eq!(entries.len(), 2);
            eprintln!("permission-denied branch not exercised: runner bypasses POSIX permissions");
        }
    }

    #[test]
    fn overlapping_and_aliased_roots_do_not_create_duplicates() {
        let dir = crate::test_support::TestDir::new();
        fs::create_dir(dir.0.join("child")).unwrap();
        dir.file("child/only.bin", b"one file");
        let root = dir.0.to_str().unwrap().to_string();
        let entries = walk_folders(
            &[root.clone(), format!("{root}/child"), format!("{root}/.")],
            true,
            0,
            |_, _| {},
        );
        assert_eq!(entries.len(), 1);
        assert!(
            crate::hashing::find_exact_duplicates(&entries, &Default::default(), |_, _| {})
                .is_empty()
        );
    }

    #[test]
    fn missing_roots_are_reported_and_valid_files_still_scanned() {
        let dir = crate::test_support::TestDir::new();
        dir.file("valid", b"data");
        let control = ScanControl::default();
        let entries = walk_folders_controlled(
            &[
                dir.0.join("missing").to_str().unwrap().into(),
                dir.0.to_str().unwrap().into(),
            ],
            true,
            0,
            |_, _| {},
            &control,
        )
        .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(control.warnings().len(), 1);
    }

    #[test]
    fn cancellation_interrupts_walking() {
        let dir = crate::test_support::TestDir::new();
        for i in 0..110 {
            dir.file(&i.to_string(), b"data");
        }
        let control = ScanControl::default();
        let result = walk_folders_controlled(
            &[dir.0.to_str().unwrap().into()],
            true,
            0,
            |_, _| {
                control
                    .cancelled
                    .store(true, std::sync::atomic::Ordering::SeqCst)
            },
            &control,
        );
        assert!(matches!(result, Err(error) if error.kind() == std::io::ErrorKind::Interrupted));
    }

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
