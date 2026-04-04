#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod bridge;
mod commands;
mod connection;
mod state;
mod tls;

use state::AppState;
use std::sync::Arc;

fn main() {
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("failed to install rustls crypto provider");

    tauri::Builder::default()
        .manage(Arc::new(AppState::new()))
        .invoke_handler(tauri::generate_handler![
            commands::generate_keypair,
            commands::load_identity,
            commands::get_public_key,
            commands::set_display_name,
            commands::get_display_name,
            commands::save_last_server,
            commands::get_last_server,
            commands::connect_server,
            commands::disconnect_server,
            commands::send_message,
            commands::fetch_history,
            commands::subscribe_channels,
            commands::get_members,
            commands::add_reaction,
            commands::remove_reaction,
            commands::create_thread,
            commands::create_invite,
            commands::create_channel,
            commands::create_category,
            commands::pick_file,
            commands::upload_file,
            commands::download_file,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
