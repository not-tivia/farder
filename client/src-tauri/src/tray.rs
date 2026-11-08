use tauri::{
    menu::{MenuBuilder, MenuItemBuilder},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle,
};

/// Build the system-tray icon. Returns an error if it could not be created, in
/// which case the app must NOT start hidden or hide on close (see
/// `window_behavior`): with no tray there would be no way back in.
pub fn setup_tray(app: &AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    let show = MenuItemBuilder::with_id("show", "Open Farder").build(app)?;
    let quit = MenuItemBuilder::with_id("quit", "Quit Farder").build(app)?;
    let menu = MenuBuilder::new(app)
        .item(&show)
        .separator()
        .item(&quit)
        .build()?;

    let mut builder = TrayIconBuilder::new()
        .tooltip("Farder")
        .menu(&menu)
        // A plain left click opens the app (the messenger habit); the menu is on
        // right click.
        .show_menu_on_left_click(false)
        .on_menu_event(move |app, event| match event.id().as_ref() {
            "show" => crate::window_behavior::show_main(app),
            "quit" => crate::window_behavior::quit_app(app),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                crate::window_behavior::show_main(tray.app_handle());
            }
        });
    // Without an icon the tray entry can be invisible. The window icon is baked in
    // at build time from icons/.
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    let _tray = builder.build(app)?;
    Ok(())
}
