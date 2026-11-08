//! "Start Farder when I sign in to Windows" (opt-in, off by default).
//!
//! It is one value in the current user's own `Run` registry key -- no admin
//! rights, nothing system-wide -- and the entry passes `--minimized`, so signing
//! in puts Farder in the tray instead of throwing a window at you. Turning it off
//! deletes exactly that value. The registry is the source of truth (the setting is
//! never cached), so what the screen shows is what Windows will do.
//!
//! Only offered in an installed (release) build: a development build's path will
//! not exist or will need a dev server at the next sign-in.

pub const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
pub const VALUE_NAME: &str = "Farder";

/// Arguments for `reg add`. The command stored is `"<exe>" --minimized`, with the
/// path quoted so a space in it ("C:\Program Files\...") cannot split it.
pub fn reg_add_args(exe: &str) -> Vec<String> {
    vec![
        "add".into(), RUN_KEY.into(),
        "/v".into(), VALUE_NAME.into(),
        "/t".into(), "REG_SZ".into(),
        "/d".into(), format!("\"{exe}\" --minimized"),
        "/f".into(),
    ]
}

pub fn reg_delete_args() -> Vec<String> {
    vec!["delete".into(), RUN_KEY.into(), "/v".into(), VALUE_NAME.into(), "/f".into()]
}

pub fn reg_query_args() -> Vec<String> {
    vec!["query".into(), RUN_KEY.into(), "/v".into(), VALUE_NAME.into()]
}

/// Why autostart cannot be offered here, or `None` if it can.
pub fn unavailable_reason() -> Option<&'static str> {
    if !cfg!(windows) {
        return Some("Starting with the computer is only supported on Windows.");
    }
    if cfg!(debug_assertions) {
        return Some("Starting with Windows is only available in an installed build, not a development build.");
    }
    None
}

#[cfg(windows)]
fn reg(args: &[String]) -> std::io::Result<std::process::Output> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    std::process::Command::new("reg").args(args).creation_flags(CREATE_NO_WINDOW).output()
}

/// Whether the sign-in entry exists right now (asks Windows, never a cached flag).
pub fn is_enabled() -> bool {
    #[cfg(windows)]
    {
        return reg(&reg_query_args()).map(|o| o.status.success()).unwrap_or(false);
    }
    #[cfg(not(windows))]
    {
        false
    }
}

pub fn set_enabled(enabled: bool) -> Result<(), String> {
    if let Some(why) = unavailable_reason() {
        return Err(why.to_string());
    }
    #[cfg(windows)]
    {
        let args = if enabled {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            reg_add_args(&exe.to_string_lossy())
        } else {
            reg_delete_args()
        };
        let out = reg(&args).map_err(|e| e.to_string())?;
        // Deleting a value that is not there is "already off", not a failure.
        if out.status.success() || (!enabled && !is_enabled()) {
            return Ok(());
        }
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    #[cfg(not(windows))]
    {
        let _ = enabled;
        Err("not supported on this platform".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stored_command_quotes_the_path_and_passes_minimized() {
        let a = reg_add_args(r"C:\Program Files\Farder\farder-client.exe");
        let data = &a[a.iter().position(|x| x == "/d").unwrap() + 1];
        assert_eq!(data, "\"C:\\Program Files\\Farder\\farder-client.exe\" --minimized");
        assert!(a.contains(&"/f".to_string()), "overwrites an old entry without prompting");
    }

    #[test]
    fn it_only_ever_touches_the_current_users_own_key_and_one_value() {
        for args in [reg_add_args("x"), reg_delete_args(), reg_query_args()] {
            assert!(args[1].starts_with(r"HKCU\"), "never HKLM: {}", args[1]);
            let v = args.iter().position(|x| x == "/v").unwrap();
            assert_eq!(args[v + 1], "Farder");
        }
    }

    #[test]
    fn a_debug_build_refuses_instead_of_registering_a_path_that_will_break() {
        if cfg!(debug_assertions) {
            assert!(unavailable_reason().is_some());
            assert!(set_enabled(true).is_err());
        }
    }
}
