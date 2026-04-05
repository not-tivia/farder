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
            commands::set_bio,
            commands::get_bio,
            commands::set_profile_color,
            commands::get_profile_color,
            commands::save_last_server,
            commands::get_last_server,
            commands::get_saved_servers,
            commands::connect_server,
            commands::disconnect_server,
            commands::list_servers,
            commands::get_server_info,
            commands::send_message,
            commands::fetch_history,
            commands::subscribe_channels,
            commands::get_members,
            commands::add_reaction,
            commands::remove_reaction,
            commands::create_thread,
            commands::search_messages,
            commands::create_invite,
            commands::create_channel,
            commands::create_category,
            commands::delete_channel,
            commands::delete_category,
            commands::update_channel,
            commands::update_category,
            commands::set_channel_override,
            commands::pick_file,
            commands::upload_file,
            commands::download_file,
            commands::fetch_url,
            commands::add_favorite,
            commands::list_favorites,
            commands::remove_favorite,
            commands::open_dm,
            commands::list_dms,
            commands::block_user,
            commands::unblock_user,
            commands::request_deletion,
            commands::cancel_deletion,
            commands::get_deletion_status,
            commands::send_typing,
            commands::edit_message,
            commands::delete_message,
            commands::assign_role,
            commands::remove_role,
            commands::create_role,
            commands::delete_role,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
