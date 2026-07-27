#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod audio;
mod audio_cpal;
mod book;
mod tenor;
mod bridge;
mod commands;
mod connection;
mod device;
mod default_relay;
mod display;
#[cfg(windows)]
mod display_wgc;
mod identity;
mod opus_codec;
mod state;
mod server_manager;
mod themes;
mod tls;
mod translation;
mod tray;
mod presence;
mod profile_sync;
mod screen_audio;
#[cfg(windows)]
mod screen_audio_wasapi;
mod screenshare;
mod video_encoder;
mod voice;
mod voice_bridge;

use state::AppState;
use std::sync::Arc;
use tauri::Manager;

fn main() {
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("failed to install rustls crypto provider");

    // Capture any farder:// deep link URL passed as a CLI argument (how most
    // desktop OS deep link handlers work — the OS re-launches the app with the
    // URL as argv[1]).
    let deep_link_url: Option<String> = std::env::args()
        .skip(1)
        .find(|a| a.starts_with("farder://"));

    tauri::Builder::default()
        .manage(Arc::new(AppState::new()))
        .manage(server_manager::ServerProcesses::new())
        .plugin(tauri_plugin_shell::init())
        .setup(move |app| {
            // Voice controller is a single global (one active call at a time).
            // Constructed here because it needs an AppHandle for event emission.
            let voice_controller = Arc::new(voice::VoiceController::new(app.handle().clone()));
            app.manage(voice_controller);

            if let Some(url) = deep_link_url {
                // Emit after a short delay so the frontend has time to mount
                // its event listener before the event fires.
                let app_handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    let _ = tauri::Emitter::emit(&app_handle, "deep-link", url);
                });
            }

            // Presence poller: samples sources every 5 s and pushes any change
            // to all connected servers.  Sources are platform-gated — on Windows
            // the GSMTC music source is active; elsewhere the list is empty and
            // the manager always returns None.
            {
                let state: Arc<AppState> = app.state::<Arc<AppState>>().inner().clone();
                tauri::async_runtime::spawn(async move {
                    let mgr = presence::PresenceManager::new(presence::default_sources());
                    loop {
                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                        let current = mgr.compute();
                        presence::push_presence_everywhere(&state, current).await;
                    }
                });
            }
            if let Err(e) = tray::setup_tray(&app.handle()) {
                eprintln!("Failed to setup tray: {}", e);
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_public_key,
            identity::identity_status,
            identity::create_identity,
            identity::unlock_identity,
            identity::migrate_plaintext_identity,
            identity::restore_identity,
            identity::save_recovery_image,
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
            commands::kick_member,
            commands::ban_member,
            commands::unban_member,
            commands::list_banned,
            commands::timeout_member,
            commands::remove_timeout,
            commands::list_audit_events,
            commands::create_role,
            commands::delete_role,
            commands::add_bot,
            commands::add_custom_bot,
            commands::remove_bot,
            commands::get_bot_poll_interval,
            commands::set_bot_poll_interval,
            commands::add_bot_alert,
            commands::remove_bot_alert,
            commands::list_bot_alerts,
            commands::subscribe_bot,
            commands::unsubscribe_bot,
            commands::list_my_subscriptions,
            commands::create_webhook,
            commands::list_webhooks,
            commands::delete_webhook,
            commands::regenerate_webhook_token,
            commands::list_commands,
            commands::add_command,
            commands::delete_command,
            commands::run_command,
            commands::update_role,
            commands::set_avatar,
            commands::get_avatar,
            commands::set_server_avatar,
            commands::get_server_avatar,
            commands::get_profile_status,
            commands::set_profile_status,
            commands::set_server_avatar_override,
            commands::clear_server_avatar_override,
            commands::get_server_avatar_override,
            commands::get_member_profile,
            commands::get_invite_preview,
            commands::get_link_embed,
            commands::get_proxied_media,
            commands::save_temp_audio,
            commands::start_recording,
            commands::stop_recording,
            commands::play_audio_file,
            commands::show_notification,
            commands::get_notification_prefs,
            commands::save_notification_prefs,
            commands::dm_encrypt,
            commands::dm_decrypt,
            commands::join_channel_media,
            commands::leave_channel_media,
            commands::get_media_state,
            commands::join_stream,
            commands::leave_stream,
            commands::enable_track,
            commands::disable_track,
            commands::set_deafen,
            commands::offer_stream_key,
            commands::join_voice,
            commands::leave_voice,
            commands::get_voice_state,
            commands::voice_join,
            commands::voice_leave,
            commands::voice_start_screen_share,
            commands::voice_stop_screen_share,
            commands::voice_request_keyframe,
            commands::list_display_sources,
            commands::list_audio_output_devices,
            commands::voice_set_mute,
            commands::voice_set_deafen,
            commands::voice_get_state,
            commands::voice_toggle_transmit,
            commands::voice_set_peer_volume,
            commands::voice_set_screen_audio_gain,
            commands::get_data_saver_embeds,
            commands::set_data_saver_embeds,
            commands::get_voice_mode,
            commands::set_voice_mode,
            commands::get_ptt_key,
            commands::set_ptt_key,
            commands::get_peer_volumes,
            commands::get_voice_sensitivity,
            commands::set_voice_sensitivity,
            commands::list_input_devices,
            commands::list_output_devices,
            commands::get_input_device,
            commands::set_input_device,
            commands::get_output_device,
            commands::set_output_device,
            commands::create_local_server,
            commands::stop_local_server,
            commands::get_local_servers,
            commands::list_templates,
            commands::restart_local_servers,
            themes::list_themes,
            themes::load_theme_css,
            themes::get_active_theme,
            themes::set_active_theme,
            themes::fork_theme,
            themes::open_themes_folder,
            themes::save_user_theme,
            themes::add_theme_asset,
            themes::delete_user_theme,
            themes::rename_user_theme,
            themes::get_theme_order,
            themes::set_theme_order,
            book::book_list_items,
            book::book_upload_item,
            book::book_delete_item,
            book::book_rename_item,
            book::book_get_file_for_server,
            book::book_migrate_legacy_favorites,
            book::book_save_from_url,
            book::book_item_absolute_path,
            tenor::tenor_search,
            tenor::tenor_trending,
            tenor::get_gif_search_settings,
            tenor::set_gif_search_settings,
            translation::get_translation_settings,
            translation::set_translation_settings,
            translation::list_available_pairs,
            translation::download_model,
            translation::list_local_models,
            translation::delete_model,
            translation::get_model_paths,
            screenshare::start_screenshare_preview,
            screenshare::stop_screenshare_preview,
            commands::get_presence_enabled,
            commands::set_presence_enabled,
            commands::get_presence_music,
            commands::set_presence_music,
            commands::submit_event,
            commands::redact_attachment,
            commands::join_log_server,
            commands::approve_member,
            commands::deny_member,
            commands::get_membership_status,
            commands::get_pending_members,
            commands::get_poll,
            commands::vote_poll,
            commands::retract_vote,
            commands::close_poll,
            commands::get_giveaway,
            commands::enter_giveaway,
            commands::leave_giveaway,
            commands::cancel_giveaway,
            commands::reroll_giveaway,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app_handle, event| {
            // Kill all locally-spawned farder-server children when the app exits,
            // so they don't pile up across sessions.
            if let tauri::RunEvent::ExitRequested { .. } | tauri::RunEvent::Exit = event {
                let procs = app_handle.state::<server_manager::ServerProcesses>();
                procs.stop_all();
            }
        });
}
