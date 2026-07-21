use crate::{
    hashing, media,
    model::{DuplicateGroup, ScanOptions, ScanProgress, ScanSummary},
    scanner,
};
use std::{collections::HashSet, sync::Mutex, time::Instant};
use tauri::{AppHandle, Emitter, Manager};

pub type ScannedFiles = Mutex<HashSet<String>>;

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
pub async fn scan(app: AppHandle, options: ScanOptions) -> Result<ScanSummary, String> {
    tauri::async_runtime::spawn_blocking(move || run_scan(app, options))
        .await
        .map_err(|e| e.to_string())?
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

/// Moves the given files to the operating system's trash/recycle bin
/// (not a permanent delete) so the action is reversible.
#[tauri::command]
pub async fn trash_files(app: AppHandle, paths: Vec<String>) -> Result<(), String> {
    require_scanned(&app, &paths)?;
    tauri::async_runtime::spawn_blocking(move || {
        trash::delete_all(&paths).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
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
