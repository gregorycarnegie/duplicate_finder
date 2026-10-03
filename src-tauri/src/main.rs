#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod fingerprint;
mod hashing;
#[cfg(test)]
mod ipc_tests;
mod media;
mod model;
mod present;
mod scan_control;
mod scanner;
#[cfg(test)]
mod test_support;

/// Everything but the context, so IPC tests drive the same state, plugins and
/// command list as the shipped app.
fn app<R: tauri::Runtime>(builder: tauri::Builder<R>) -> tauri::Builder<R> {
    builder
        .manage(commands::ScannedFiles::default())
        .manage(scan_control::Operations::default())
        .manage(commands::LastSummary::default())
        .manage(commands::PermanentCandidates::default())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            commands::pick_folders,
            commands::folders_from_paths,
            commands::open_file,
            commands::reveal_file,
            commands::scan,
            commands::cancel_scan,
            commands::trash_files,
            commands::delete_files_permanently,
        ])
}

fn main() {
    app(tauri::Builder::default())
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
