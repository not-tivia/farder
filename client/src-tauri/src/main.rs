#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod commands;
mod state;
use state::AppState;

fn main() {
    tauri::Builder::default()
        .manage(AppState::new())
        .invoke_handler(tauri::generate_handler![
            commands::generate_keypair,
            commands::get_public_key,
            commands::export_key,
            commands::import_key,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
