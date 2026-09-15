// The update check, amendment A31: a daily look at GitHub's releases, a notification
// and a tray item when there is something newer, and nothing else. The result lives in
// `AppState` so the About screen can show it, and the same screen can ask by hand
// whether or not the automatic check is on.

use crate::{notify_user, refresh_tray, AppState};
use lathe_core::update::Available;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Manager};

pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

/// What the About screen shows: the running version and, once a check has run, what
/// it found. `checked` is false until the first check finishes, so the screen can tell
/// "nothing newer" from "not looked yet".
#[derive(Clone, Default, serde::Serialize)]
pub struct Status {
    pub current: &'static str,
    pub checked: bool,
    pub available: Option<Available>,
    /// The last check's failure, if it failed. Offline is the common case and is not
    /// worth a notification, but the screen should be able to say so.
    pub error: Option<String>,
}

pub type Shared = Arc<Mutex<Status>>;

pub fn shared() -> Shared {
    Arc::new(Mutex::new(Status {
        current: CURRENT,
        ..Default::default()
    }))
}

/// Runs the check and records the outcome. The notification and the tray item only
/// appear the first time a given version is seen, so a daily re-check does not nag.
pub fn check_now(app: &AppHandle) -> Status {
    let state = app.state::<AppState>();
    let found = lathe_core::update::check(CURRENT);
    let mut status = state.update.lock().unwrap();
    status.checked = true;
    match found {
        Ok(available) => {
            let is_new = available.is_some() && available != status.available;
            if available.is_none() {
                eprintln!("update: {CURRENT} is the latest release");
            }
            status.available = available;
            status.error = None;
            if is_new {
                let version = &status.available.as_ref().expect("checked above").version;
                eprintln!("update: Lathe {version} is available");
                notify_user(
                    &format!("Lathe {version} is available"),
                    "Open the tray menu or the About screen to get it.",
                );
                drop(status);
                refresh_tray(app);
                return state.update.lock().unwrap().clone();
            }
        }
        Err(e) => {
            eprintln!("update check failed: {e:#}");
            status.error = Some(format!("{e:#}"));
        }
    }
    status.clone()
}

/// The automatic check: shortly after start, then daily. Reads the switch on every
/// round so turning it off in settings takes effect without a restart, and never
/// touches the network while it is off.
pub fn spawn_checker(app: AppHandle) {
    std::thread::spawn(move || {
        // Not at once: startup is the moment the user is most likely to press the
        // hotkey, and a network round trip has no business next to the first load.
        std::thread::sleep(Duration::from_secs(20));
        loop {
            let enabled = app
                .try_state::<AppState>()
                .map(|s| s.config.lock().unwrap().updates.check)
                .unwrap_or(false);
            if enabled {
                check_now(&app);
            }
            std::thread::sleep(Duration::from_secs(24 * 60 * 60));
        }
    });
}

/// The release page in the user's browser. Platform shells rather than a crate: it is
/// one URL, opened rarely.
pub fn open_release_page(url: &str) {
    if !url.starts_with("https://github.com/") {
        eprintln!("refusing to open a release page outside github.com: {url}");
        return;
    }
    #[cfg(windows)]
    let result = {
        use windows::core::{HSTRING, PCWSTR};
        use windows::Win32::UI::Shell::ShellExecuteW;
        use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
        let verb = HSTRING::from("open");
        let target = HSTRING::from(url);
        let handle = unsafe {
            ShellExecuteW(
                None,
                PCWSTR(verb.as_ptr()),
                PCWSTR(target.as_ptr()),
                PCWSTR::null(),
                PCWSTR::null(),
                SW_SHOWNORMAL,
            )
        };
        // ShellExecute reports success as a value above 32.
        if handle.0 as usize > 32 {
            Ok(())
        } else {
            Err(format!("ShellExecute returned {}", handle.0 as usize))
        }
    };
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open")
        .arg(url)
        .status()
        .map(|_| ())
        .map_err(|e| e.to_string());
    #[cfg(target_os = "linux")]
    let result = std::process::Command::new("xdg-open")
        .arg(url)
        .status()
        .map(|_| ())
        .map_err(|e| e.to_string());
    if let Err(e) = result {
        eprintln!("could not open {url}: {e}");
        notify_user("Lathe", &format!("Could not open the browser. The release is at {url}"));
    }
}
