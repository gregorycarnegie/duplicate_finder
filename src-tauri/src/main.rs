#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod hashing;
mod media;
mod model;
mod scanner;

fn main() {
    tauri::Builder::default()
        .manage(commands::ScannedFiles::default())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            commands::pick_folders,
            commands::folders_from_paths,
            commands::open_file,
            commands::reveal_file,
            commands::scan,
            commands::trash_files,
            commands::delete_files_permanently,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
