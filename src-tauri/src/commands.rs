use crate::{
    hashing, media,
    model::{self, DuplicateGroup, ScanOptions, ScanProgress, ScanSummary},
    present::{self, ScanSummaryView},
    scanner,
};
use std::{collections::HashSet, sync::Mutex, time::Instant};
use tauri::{AppHandle, Emitter, Manager};

pub type ScannedFiles = Mutex<HashSet<String>>;
pub type LastSummary = Mutex<Option<ScanSummary>>;

fn all_scanned(scanned: &HashSet<String>, paths: &[String]) -> bool {
    paths.iter().all(|path| scanned.contains(path))
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
pub async fn scan(app: AppHandle, options: ScanOptions) -> Result<ScanSummaryView, String> {
    let scan_app = app.clone();
    let summary = tauri::async_runtime::spawn_blocking(move || run_scan(scan_app, options))
        .await
        .map_err(|e| e.to_string())??;

    let view = present::present_summary(&summary);
    let last = app.state::<LastSummary>();
    *last.lock().map_err(|e| e.to_string())? = Some(summary);
    Ok(view)
}

fn run_scan(app: AppHandle, options: ScanOptions) -> Result<ScanSummary, String> {
    let start = Instant::now();

    if options.folders.is_empty() {
        return Err("Add at least one folder to scan.".into());
    }

    let ffmpeg_available = media::init();

    let entries = scanner::walk_folders(
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
    );

    {
        let scanned = app.state::<ScannedFiles>();
        let mut scanned = scanned.lock().map_err(|e| e.to_string())?;
        *scanned = entries.iter().map(|entry| entry.path.clone()).collect();
    }

    let files_scanned = entries.len() as u64;
    let bytes_scanned: u64 = entries.iter().map(|e| e.size).sum();

    let media_lookup = if ffmpeg_available {
        media::probe_all(&entries, |done, total| {
            let _ = app.emit("scan-progress", ScanProgress::Probing { done, total });
        })
    } else {
        Default::default()
    };

    let exact_groups: Vec<DuplicateGroup> =
        hashing::find_exact_duplicates(&entries, &media_lookup, |done, total| {
            let _ = app.emit("scan-progress", ScanProgress::Hashing { done, total });
        });

    let exact_paths: HashSet<&str> = exact_groups
        .iter()
        .flat_map(|g| g.files.iter().map(|f| f.entry.path.as_str()))
        .collect();

    let media_groups = if ffmpeg_available {
        media::cluster_by_duration(
            &entries,
            &media_lookup,
            options.duration_tolerance_secs,
            &exact_paths,
        )
    } else {
        Vec::new()
    };

    let reclaimable_bytes = exact_groups
        .iter()
        .map(|g| g.reclaimable_bytes)
        .sum::<u64>()
        + media_groups
            .iter()
            .map(|g| g.reclaimable_bytes)
            .sum::<u64>();

    Ok(ScanSummary {
        files_scanned,
        bytes_scanned,
        exact_groups,
        media_groups,
        reclaimable_bytes,
        elapsed_ms: start.elapsed().as_millis() as u64,
        ffmpeg_available,
    })
}

#[derive(serde::Serialize)]
pub struct OpFailure {
    path: String,
    error: String,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrashResult {
    summary: ScanSummaryView,
    failures: Vec<OpFailure>,
}

/// Removes the successfully-processed paths from the last scan's groups and
/// returns the updated, display-ready summary alongside any failures.
fn apply_removal(
    app: &AppHandle,
    paths: &[String],
    failures: Vec<OpFailure>,
) -> Result<TrashResult, String> {
    let failed: HashSet<&str> = failures.iter().map(|f| f.path.as_str()).collect();
    let removed: HashSet<String> = paths
        .iter()
        .filter(|p| !failed.contains(p.as_str()))
        .cloned()
        .collect();

    let last = app.state::<LastSummary>();
    let mut last = last.lock().map_err(|e| e.to_string())?;
    let summary = last
        .as_mut()
        .ok_or_else(|| "No scan in progress.".to_string())?;
    model::remove_paths(summary, &removed);

    Ok(TrashResult {
        summary: present::present_summary(summary),
        failures,
    })
}

/// Moves the given files to the operating system's trash/recycle bin
/// (not a permanent delete) so the action is reversible. Paths that don't
/// support trashing (e.g. network shares/NAS mounts) are reported back as
/// failures instead of aborting the whole batch.
#[tauri::command]
pub async fn trash_files(app: AppHandle, paths: Vec<String>) -> Result<TrashResult, String> {
    require_scanned(&app, &paths)?;
    let move_paths = paths.clone();
    let failures = tauri::async_runtime::spawn_blocking(move || {
        move_paths
            .into_iter()
            .filter_map(|path| {
                trash::delete(&path).err().map(|e| OpFailure {
                    path,
                    error: e.to_string(),
                })
            })
            .collect::<Vec<_>>()
    })
    .await
    .map_err(|e| e.to_string())?;

    apply_removal(&app, &paths, failures)
}

/// Permanently deletes files, bypassing the trash. Intended as a fallback
/// for paths where `trash_files` fails (no recycle bin support), so callers
/// should only invoke this after the user has confirmed the operation is
/// irreversible.
#[tauri::command]
pub async fn delete_files_permanently(
    app: AppHandle,
    paths: Vec<String>,
) -> Result<TrashResult, String> {
    require_scanned(&app, &paths)?;
    let delete_paths = paths.clone();
    let failures = tauri::async_runtime::spawn_blocking(move || {
        delete_paths
            .into_iter()
            .filter_map(|path| {
                std::fs::remove_file(&path).err().map(|e| OpFailure {
                    path,
                    error: e.to_string(),
                })
            })
            .collect::<Vec<_>>()
    })
    .await
    .map_err(|e| e.to_string())?;

    apply_removal(&app, &paths, failures)
}

#[cfg(test)]
mod tests {
    use super::{all_scanned, folders_from_paths};
    use std::collections::HashSet;

    #[test]
    fn dropped_files_are_not_added_as_folders() {
        let folder = env!("CARGO_MANIFEST_DIR").to_string();
        let file = format!("{folder}/Cargo.toml");

        assert_eq!(folders_from_paths(vec![folder.clone(), file]), vec![folder]);
    }

    #[test]
    fn only_scanned_files_are_allowed() {
        let scanned = HashSet::from(["scanned".to_string()]);

        assert!(all_scanned(&scanned, &["scanned".to_string()]));
        assert!(!all_scanned(&scanned, &["other".to_string()]));
    }
}
