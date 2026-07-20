mod commands;
mod hashing;
mod media;
mod model;
mod scanner;

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            commands::pick_folders,
            commands::scan,
            commands::trash_files,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
