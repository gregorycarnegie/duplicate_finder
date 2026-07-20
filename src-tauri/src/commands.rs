use crate::model::{DuplicateGroup, ScanOptions, ScanProgress, ScanSummary};
use crate::{hashing, media, scanner};
use std::collections::HashSet;
use std::time::Instant;
use tauri::{AppHandle, Emitter};

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

    let files_scanned = entries.len() as u64;
    let bytes_scanned: u64 = entries.iter().map(|e| e.size).sum();

    let media_lookup = if ffmpeg_available {
        media::probe_all(&entries, |done, total| {
            let _ = app.emit("scan-progress", ScanProgress::Probing { done, total });
        })
    } else {
        Default::default()
    };

    let exact_groups: Vec<DuplicateGroup> = hashing::find_exact_duplicates(
        &entries,
        &media_lookup,
        |done, total| {
            let _ = app.emit("scan-progress", ScanProgress::Hashing { done, total });
        },
    );

    let exact_paths: HashSet<String> = exact_groups
        .iter()
        .flat_map(|g| g.files.iter().map(|f| f.entry.path.clone()))
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

    let reclaimable_bytes = exact_groups.iter().map(|g| g.reclaimable_bytes).sum::<u64>()
        + media_groups.iter().map(|g| g.reclaimable_bytes).sum::<u64>();

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
pub async fn trash_files(paths: Vec<String>) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || trash::delete_all(&paths).map_err(|e| e.to_string()))
        .await
        .map_err(|e| e.to_string())?
}
