// What each platform needs from the user before a dictation can happen, amendment
// A32. Windows needs nothing. macOS needs the Accessibility permission, and a tap
// created before it was granted stays dead, so the app watches for the grant and
// restarts itself. Linux needs the user in the `input` group and a udev rule for
// /dev/uinput; the .deb installs the rule, the AppImage cannot, and neither can add
// the user to a group, so the app says exactly what to run.
//
// Everything here is reported twice: as a notification at startup, because the tray
// icon is otherwise the only sign anything happened, and through `problems()` to the
// Hotkeys screen, where the full text fits.

use tauri::AppHandle;

/// One thing the user has to do, in words the Hotkeys screen shows as-is.
#[derive(Clone, serde::Serialize)]
pub struct Problem {
    pub title: String,
    pub detail: String,
    /// A command or a click path. Shown in a monospace block.
    pub fix: String,
}

#[cfg(windows)]
pub fn problems() -> Vec<Problem> {
    Vec::new()
}

#[cfg(target_os = "macos")]
pub fn problems() -> Vec<Problem> {
    if lathe_core::hotkey::macos_accessibility_trusted(false) {
        return Vec::new();
    }
    vec![Problem {
        title: "Accessibility permission".into(),
        detail: "macOS only lets Lathe hear the hotkey and type into other apps once it \
                 is allowed under Accessibility. Lathe restarts by itself when the \
                 permission is granted."
            .into(),
        fix: "System Settings > Privacy & Security > Accessibility > enable Lathe".into(),
    }]
}

#[cfg(target_os = "linux")]
pub fn problems() -> Vec<Problem> {
    let mut found = Vec::new();
    if !lathe_core::hotkey::linux_keyboard_readable() {
        found.push(Problem {
            title: "Keyboard not readable".into(),
            detail: "Lathe reads the keyboard through /dev/input, which needs your user \
                     in the input group. Log out and back in after adding it."
                .into(),
            fix: "sudo usermod -aG input $USER".into(),
        });
    }
    if std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/uinput")
        .is_err()
    {
        found.push(Problem {
            title: "Paste keystroke unavailable".into(),
            detail: "Lathe presses Ctrl+V through a virtual keyboard, which needs write \
                     access to /dev/uinput. The .deb installs this rule; the AppImage \
                     cannot."
                .into(),
            fix: "echo 'KERNEL==\"uinput\", GROUP=\"input\", MODE=\"0660\"' | sudo tee \
                  /etc/udev/rules.d/70-lathe.rules && sudo udevadm control --reload-rules \
                  && sudo udevadm trigger --subsystem-match=misc"
                .into(),
        });
    }
    found
}

/// Says what is missing, once, when the app starts. On macOS also shows the system's
/// own Accessibility prompt and waits for the answer.
pub fn check_at_startup(app: AppHandle) {
    let found = problems();
    if found.is_empty() {
        return;
    }
    for p in &found {
        eprintln!("setup: {} -- {} Fix: {}", p.title, p.detail, p.fix);
    }
    let titles = found
        .iter()
        .map(|p| p.title.as_str())
        .collect::<Vec<_>>()
        .join("; ");
    crate::notify_user(
        "Lathe needs one more thing",
        &format!("{titles}. Open Settings > Hotkeys for the steps."),
    );
    platform_follow_up(app);
}

/// The system prompt, then a poll for the grant, then a restart. The poll rather than
/// a callback: macOS has no notification for a TCC change, and every app that needs
/// this permission does the same.
#[cfg(target_os = "macos")]
fn platform_follow_up(app: AppHandle) {
    use std::time::Duration;
    lathe_core::hotkey::macos_accessibility_trusted(true);
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(2));
        if lathe_core::hotkey::macos_accessibility_trusted(false) {
            eprintln!("setup: Accessibility granted; restarting");
            crate::notify_user(
                "Lathe",
                "Accessibility granted. Restarting so the hotkey works.",
            );
            relaunch(&app);
            return;
        }
    });
}

/// Starts a fresh copy after this one has gone. Not at once: the single-instance
/// guard would hand the new copy's launch to this one and exit it. A bundled app is
/// relaunched through `open`, which keeps the bundle identity the permission was
/// granted to; a bare binary is simply run again.
#[cfg(target_os = "macos")]
fn relaunch(app: &AppHandle) {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let exe = exe.display().to_string();
    let launch = match exe.find(".app/Contents/MacOS/") {
        Some(end) => format!("open -n \"{}\"", &exe[..end + 4]),
        None => format!("\"{exe}\""),
    };
    let _ = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("sleep 1; {launch}"))
        .spawn();
    app.exit(0);
}

#[cfg(not(target_os = "macos"))]
fn platform_follow_up(_app: AppHandle) {}
