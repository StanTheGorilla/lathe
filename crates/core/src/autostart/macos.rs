// macOS: a launch agent in ~/Library/LaunchAgents. launchd reads the folder at login,
// and Login Items in System Settings lists what it finds there, so the user can see and
// remove it without Lathe's help.

use anyhow::{Context as _, Result};
use std::path::PathBuf;

const LABEL: &str = "dev.lathe.app";

fn plist_path() -> Result<PathBuf> {
    let home = dirs::home_dir().context("no home directory")?;
    Ok(home
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{LABEL}.plist")))
}

pub(super) fn is_enabled() -> bool {
    plist_path().map(|p| p.exists()).unwrap_or(false)
}

pub(super) fn set_enabled(enabled: bool) -> Result<()> {
    let path = plist_path()?;
    if !enabled {
        // Absent is the desired state, so a missing file is success, not failure.
        return match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).context("removing the launch agent"),
        };
    }
    let exe = std::env::current_exe().context("locating the executable")?;
    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
</dict>
</plist>
"#,
        exe = xml_escape(&exe.display().to_string())
    );
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).context("creating ~/Library/LaunchAgents")?;
    }
    std::fs::write(&path, plist).context("writing the launch agent")
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}
