use crate::{
    hashing, media,
    model::{self, DuplicateGroup, ScanOptions, ScanProgress, ScanSummary},
    present::{self, ScanSummaryView},
    scan_control::{FileSnapshot, FileStamp, Operations, ScanControl},
    scanner,
};
use std::{
    collections::{HashMap, HashSet},
    sync::{atomic::Ordering, Mutex},
    time::Instant,
};
use tauri::{AppHandle, Emitter, Manager};

pub type ScannedFiles = Mutex<HashMap<String, FileSnapshot>>;
pub type PermanentCandidates = Mutex<HashSet<String>>;
pub type LastSummary = Mutex<Option<ScanSummary>>;

fn all_scanned(scanned: &HashMap<String, FileSnapshot>, paths: &[String]) -> bool {
    paths.iter().all(|path| scanned.contains_key(path))
}

fn require_scanned(app: &AppHandle, paths: &[String]) -> Result<(), String> {
    let scanned = app.state::<ScannedFiles>();
    let scanned = scanned.lock().map_err(|e| e.to_string())?;
    all_scanned(&scanned, paths)
        .then_some(())
        .ok_or_else(|| "That file is not part of the latest scan.".into())
}

#[tauri::command]
pub async fn pick_folders(app: AppHandle) -> Result<Vec<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    let folders = app.dialog().file().blocking_pick_folders();
    Ok(folders
        .unwrap_or_default()
        .into_iter()
        .filter_map(|p| p.into_path().ok())
        .map(|p| p.to_string_lossy().to_string())
        .collect())
}

#[tauri::command]
pub fn folders_from_paths(paths: Vec<String>) -> Vec<String> {
    paths
        .into_iter()
        .filter(|path| std::path::Path::new(path).is_dir())
        .collect()
}

#[tauri::command]
pub fn open_file(app: AppHandle, path: String) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    require_scanned(&app, std::slice::from_ref(&path))?;
    app.opener()
        .open_path(path, None::<&str>)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn reveal_file(app: AppHandle, path: String) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    require_scanned(&app, std::slice::from_ref(&path))?;
    app.opener()
        .reveal_item_in_dir(path)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn cancel_scan(app: AppHandle) {
    app.state::<Operations>()
        .cancelled
        .store(true, Ordering::SeqCst);
}

#[tauri::command]
pub async fn scan(app: AppHandle, options: ScanOptions) -> Result<ScanSummaryView, String> {
    let operations = app.state::<Operations>();
    let guard = operations.begin()?;
    let control = ScanControl {
        cancelled: operations.cancelled.clone(),
        ..Default::default()
    };
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = guard;
        app.state::<ScannedFiles>()
            .lock()
            .map_err(|e| e.to_string())?
            .clear();
        app.state::<PermanentCandidates>()
            .lock()
            .map_err(|e| e.to_string())?
            .clear();
        *app.state::<LastSummary>()
            .lock()
            .map_err(|e| e.to_string())? = None;
        let (summary, snapshots) = run_scan(&app, options, &control)?;
        control.check().map_err(|e| e.to_string())?;
        let view = present::present_summary(&summary);
        *app.state::<ScannedFiles>()
            .lock()
            .map_err(|e| e.to_string())? = snapshots;
        *app.state::<LastSummary>()
            .lock()
            .map_err(|e| e.to_string())? = Some(summary);
        Ok(view)
    })
    .await
    .map_err(|e| e.to_string())?
}

fn run_scan(
    app: &AppHandle,
    options: ScanOptions,
    control: &ScanControl,
) -> Result<(ScanSummary, HashMap<String, FileSnapshot>), String> {
    let start = Instant::now();
    if options.folders.is_empty() {
        return Err("Add at least one folder to scan.".into());
    }
    if !options.duration_tolerance_secs.is_finite()
        || !(0.0..=5.0).contains(&options.duration_tolerance_secs)
    {
        return Err("Duration tolerance must be between 0 and 5 seconds.".into());
    }
    let entries = scanner::walk_folders_controlled(
        &options.folders,
        options.include_hidden,
        options.min_file_size,
        |folder, files_found| {
            let _ = app.emit(
                "scan-progress",
                ScanProgress::Walking {
                    folder: folder.to_string(),
                    files_found,
                },
            );
        },
        control,
    )
    .map_err(|e| e.to_string())?;
    let ffmpeg_available = media::init(control);
    control.check().map_err(|e| e.to_string())?;
    let media_lookup = if ffmpeg_available {
        media::probe_all(
            &entries,
            |done, total| {
                let _ = app.emit("scan-progress", ScanProgress::Probing { done, total });
            },
            control,
        )
    } else {
        Default::default()
    };
    control.check().map_err(|e| e.to_string())?;
    let exact_groups = hashing::find_exact_duplicates_controlled(
        &entries,
        &media_lookup,
        |done, total| {
            let _ = app.emit("scan-progress", ScanProgress::Hashing { done, total });
        },
        control,
    );
    control.check().map_err(|e| e.to_string())?;
    let exact_paths: HashSet<&str> = exact_groups
        .iter()
        .flat_map(|g| g.files.iter().map(|f| f.entry.path.as_str()))
        .collect();
    let media_groups = media::cluster_by_duration(
        &entries,
        &media_lookup,
        options.duration_tolerance_secs,
        &exact_paths,
    );
    let mut summary = ScanSummary {
        files_scanned: entries.len() as u64,
        bytes_scanned: entries.iter().map(|e| e.size).sum(),
        exact_groups,
        media_groups,
        reclaimable_bytes: 0,
        elapsed_ms: 0,
        ffmpeg_available,
        warnings: vec![],
    };
    let paths: Vec<String> = summary
        .exact_groups
        .iter()
        .chain(&summary.media_groups)
        .flat_map(|g| g.files.iter().map(|f| f.entry.path.clone()))
        .collect();
    let mut snapshots = HashMap::new();
    let mut invalid = HashSet::new();
    for (index, path) in paths.iter().enumerate() {
        control.check().map_err(|e| e.to_string())?;
        let _ = app.emit(
            "scan-progress",
            ScanProgress::Verifying {
                done: index as u64,
                total: paths.len() as u64,
            },
        );
        let result = (|| -> Result<FileSnapshot, String> {
            let stamp = control
                .stamps
                .lock()
                .unwrap()
                .get(path)
                .cloned()
                .ok_or("Missing file snapshot")?;
            if FileStamp::read(path).map_err(|e| e.to_string())? != stamp {
                return Err("File changed during scanning; scan again".into());
            }
            let cached = control.hashes.lock().unwrap().get(path).copied();
            let hash = match cached {
                Some(hash) => hash,
                None => hashing::hash_file_controlled(path, control).map_err(|e| e.to_string())?,
            };
            if FileStamp::read(path).map_err(|e| e.to_string())? != stamp {
                return Err("File changed during scanning; scan again".into());
            }
            Ok(FileSnapshot { stamp, hash })
        })();
        match result {
            Ok(snapshot) => {
                snapshots.insert(path.clone(), snapshot);
            }
            Err(error) => {
                control.warn(path, error);
                invalid.insert(path.clone());
            }
        }
    }
    control.check().map_err(|e| e.to_string())?;
    model::remove_paths(&mut summary, &invalid);
    summary.elapsed_ms = start.elapsed().as_millis() as u64;
    summary.warnings = control.warnings();
    Ok((summary, snapshots))
}

#[derive(serde::Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct OpFailure {
    path: String,
    error: String,
    can_delete_permanently: bool,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrashResult {
    summary: ScanSummaryView,
    failures: Vec<OpFailure>,
}

fn verify_snapshot(path: &str, snapshot: &FileSnapshot) -> Result<(), String> {
    let changed = "File changed since scanning. Run a new scan before removing it.";
    if FileStamp::read(path).map_err(|e| e.to_string())? != snapshot.stamp {
        return Err(changed.into());
    }
    let hash =
        hashing::hash_file_controlled(path, &ScanControl::default()).map_err(|e| e.to_string())?;
    if hash != snapshot.hash || FileStamp::read(path).map_err(|e| e.to_string())? != snapshot.stamp
    {
        return Err(changed.into());
    }
    Ok(())
}

// Validate both the selected file and a retained group member immediately before
// removal. Filesystem changes racing the final OS operation remain possible.
fn remove_checked<F: FnMut(&str) -> Result<(), String>>(
    paths: &[String],
    snapshots: &HashMap<String, FileSnapshot>,
    groups: &[DuplicateGroup],
    permanent: bool,
    mut remove: F,
) -> (HashSet<String>, Vec<OpFailure>) {
    let selected: HashSet<&str> = paths.iter().map(String::as_str).collect();
    let mut removed = HashSet::new();
    let mut failures = Vec::new();
    let mut seen = HashSet::new();
    for path in paths {
        if !seen.insert(path) {
            continue;
        }
        let validation = (|| -> Result<(), String> {
            let snapshot = snapshots
                .get(path)
                .ok_or("File is not part of the latest scan")?;
            verify_snapshot(path, snapshot)?;
            let group = groups
                .iter()
                .find(|g| g.files.iter().any(|f| &f.entry.path == path))
                .ok_or("File no longer belongs to a duplicate or comparison group")?;
            let keeper = group.files.iter().any(|file| {
                !selected.contains(file.entry.path.as_str())
                    && snapshots
                        .get(&file.entry.path)
                        .is_some_and(|snapshot| verify_snapshot(&file.entry.path, snapshot).is_ok())
            });
            if !keeper {
                return Err("Keep at least one unchanged file in each group. Run a new scan if a retained file changed.".into());
            }
            if FileStamp::read(path).map_err(|e| e.to_string())? != snapshot.stamp {
                return Err("File changed during verification. Run a new scan.".into());
            }
            Ok(())
        })();
        if let Err(error) = validation {
            failures.push(OpFailure {
                path: path.clone(),
                error,
                can_delete_permanently: false,
            });
            continue;
        }
        match remove(path) {
            Ok(()) => {
                removed.insert(path.clone());
            }
            Err(error) => failures.push(OpFailure {
                path: path.clone(),
                error,
                can_delete_permanently: !permanent,
            }),
        }
    }
    (removed, failures)
}

async fn remove_files(
    app: AppHandle,
    paths: Vec<String>,
    permanent: bool,
) -> Result<TrashResult, String> {
    let guard = app.state::<Operations>().begin()?;
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = guard;
        if permanent
            && !paths.iter().all(|p| {
                app.state::<PermanentCandidates>()
                    .lock()
                    .unwrap()
                    .contains(p)
            })
        {
            return Err(
                "Permanent deletion is only available for files that could not be trashed.".into(),
            );
        }
        let mut last = app
            .state::<LastSummary>()
            .lock()
            .map_err(|e| e.to_string())?
            .clone()
            .ok_or("Run a scan before removing files")?;
        let snapshots = app
            .state::<ScannedFiles>()
            .lock()
            .map_err(|e| e.to_string())?
            .clone();
        let groups: Vec<_> = last
            .exact_groups
            .iter()
            .chain(&last.media_groups)
            .cloned()
            .collect();
        let (removed, failures) = remove_checked(&paths, &snapshots, &groups, permanent, |path| {
            if permanent {
                std::fs::remove_file(path).map_err(|e| e.to_string())
            } else {
                trash::delete(path).map_err(|e| e.to_string())
            }
        });
        model::remove_paths(&mut last, &removed);
        app.state::<ScannedFiles>()
            .lock()
            .map_err(|e| e.to_string())?
            .retain(|p, _| !removed.contains(p));
        {
            let candidates = app.state::<PermanentCandidates>();
            let mut candidates = candidates.lock().map_err(|e| e.to_string())?;
            candidates.clear();
            candidates.extend(
                failures
                    .iter()
                    .filter(|f| f.can_delete_permanently)
                    .map(|f| f.path.clone()),
            );
        }
        let view = present::present_summary(&last);
        *app.state::<LastSummary>()
            .lock()
            .map_err(|e| e.to_string())? = Some(last);
        Ok(TrashResult {
            summary: view,
            failures,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn trash_files(app: AppHandle, paths: Vec<String>) -> Result<TrashResult, String> {
    remove_files(app, paths, false).await
}

#[tauri::command]
pub async fn delete_files_permanently(
    app: AppHandle,
    paths: Vec<String>,
) -> Result<TrashResult, String> {
    remove_files(app, paths, true).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        model::{DuplicateFile, FileEntry},
        test_support::TestDir,
    };

    fn fixture() -> (
        TestDir,
        HashMap<String, FileSnapshot>,
        Vec<DuplicateGroup>,
        Vec<String>,
    ) {
        let dir = TestDir::new();
        let paths = vec![
            dir.file("a", b"same"),
            dir.file("b", b"same"),
            dir.file("c", b"same"),
        ];
        let snapshots = paths
            .iter()
            .map(|p| {
                (
                    p.clone(),
                    FileSnapshot {
                        stamp: FileStamp::read(p).unwrap(),
                        hash: hashing::hash_file(p).unwrap(),
                    },
                )
            })
            .collect();
        let groups = vec![DuplicateGroup {
            files: paths
                .iter()
                .map(|p| DuplicateFile {
                    entry: FileEntry {
                        path: p.clone(),
                        size: 4,
                        modified: None,
                    },
                    media: None,
                })
                .collect(),
            reclaimable_bytes: 8,
        }];
        (dir, snapshots, groups, paths)
    }

    #[test]
    fn removal_deletes_selected_files_and_retains_a_copy() {
        let (_dir, snapshots, groups, paths) = fixture();
        let (removed, failures) = remove_checked(&paths[..2], &snapshots, &groups, true, |p| {
            std::fs::remove_file(p).map_err(|e| e.to_string())
        });
        assert_eq!(removed.len(), 2);
        assert!(failures.is_empty());
        assert!(!std::path::Path::new(&paths[0]).exists());
        assert_eq!(std::fs::read(&paths[2]).unwrap(), b"same");
    }

    #[test]
    fn cannot_remove_every_copy_or_an_unscanned_file() {
        let (dir, snapshots, groups, mut paths) = fixture();
        paths.push(dir.file("unscanned", b"unique"));
        let (removed, failures) = remove_checked(&paths, &snapshots, &groups, false, |_| {
            panic!("must not remove")
        });
        assert!(removed.is_empty());
        assert_eq!(failures.len(), 4);
        assert!(failures.iter().all(|f| !f.can_delete_permanently));
    }

    #[test]
    fn same_size_content_changes_are_rejected_even_with_matching_metadata() {
        let (_dir, mut snapshots, groups, paths) = fixture();
        std::fs::write(&paths[0], b"edit").unwrap();
        // Force equal metadata to independently exercise the content check.
        snapshots.get_mut(&paths[0]).unwrap().stamp = FileStamp::read(&paths[0]).unwrap();
        let (removed, failures) = remove_checked(&paths[..1], &snapshots, &groups, false, |_| {
            panic!("must not remove")
        });
        assert!(removed.is_empty());
        assert_eq!(failures.len(), 1);
        assert!(!failures[0].can_delete_permanently);
    }

    #[test]
    fn changed_or_missing_keeper_blocks_removal() {
        let (_dir, snapshots, groups, paths) = fixture();
        std::fs::write(&paths[2], b"new content").unwrap();
        let (_, failures) = remove_checked(&paths[..2], &snapshots, &groups, true, |_| {
            panic!("must not remove")
        });
        assert_eq!(failures.len(), 2);
        std::fs::remove_file(&paths[2]).unwrap();
        let (_, failures) = remove_checked(&paths[..2], &snapshots, &groups, true, |_| {
            panic!("must not remove")
        });
        assert_eq!(failures.len(), 2);
    }

    #[test]
    fn partial_trash_failure_only_offers_fallback_for_os_failures() {
        let (_dir, snapshots, groups, paths) = fixture();
        let (removed, failures) = remove_checked(&paths[..2], &snapshots, &groups, false, |p| {
            if p == paths[0] {
                Err("Trash not supported".into())
            } else {
                std::fs::remove_file(p).map_err(|e| e.to_string())
            }
        });
        assert_eq!(removed, HashSet::from([paths[1].clone()]));
        assert_eq!(failures.len(), 1);
        assert!(failures[0].can_delete_permanently);
        assert_eq!(failures[0].error, "Trash not supported");
        std::fs::write(&paths[0], b"edit").unwrap();
        let (_, failures) = remove_checked(&paths[..1], &snapshots, &groups, true, |_| {
            panic!("must not remove")
        });
        assert_eq!(failures.len(), 1);
        assert!(!failures[0].can_delete_permanently);
    }

    #[test]
    fn repeated_selection_is_processed_once() {
        let (_dir, snapshots, groups, paths) = fixture();
        let mut calls = 0;
        let (removed, failures) = remove_checked(
            &[paths[0].clone(), paths[0].clone()],
            &snapshots,
            &groups,
            false,
            |_| {
                calls += 1;
                Ok(())
            },
        );
        assert_eq!(calls, 1);
        assert_eq!(removed.len(), 1);
        assert!(failures.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn replaced_symlink_is_not_deleted() {
        let (dir, snapshots, groups, paths) = fixture();
        let target = dir.file("valuable", b"valuable content");
        std::fs::remove_file(&paths[0]).unwrap();
        std::os::unix::fs::symlink(&target, &paths[0]).unwrap();
        let (_, failures) = remove_checked(&paths[..1], &snapshots, &groups, true, |_| {
            panic!("must not remove")
        });
        assert_eq!(failures.len(), 1);
        assert_eq!(std::fs::read(target).unwrap(), b"valuable content");
    }

    #[test]
    fn dropped_files_are_not_added_as_folders() {
        let folder = env!("CARGO_MANIFEST_DIR").to_string();
        let file = format!("{folder}/Cargo.toml");
        assert_eq!(folders_from_paths(vec![folder.clone(), file]), vec![folder]);
    }
}
