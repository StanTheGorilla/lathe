// Linux: a desktop entry in the XDG autostart folder, which every desktop environment
// reads at login and most list under their startup settings.

use anyhow::{Context as _, Result};
use std::path::PathBuf;

fn entry_path() -> Result<PathBuf> {
    let config = dirs::config_dir().context("no config directory")?;
    Ok(config.join("autostart").join("lathe.desktop"))
}

pub(super) fn is_enabled() -> bool {
    entry_path().map(|p| p.exists()).unwrap_or(false)
}

pub(super) fn set_enabled(enabled: bool) -> Result<()> {
    let path = entry_path()?;
    if !enabled {
        // Absent is the desired state, so a missing file is success, not failure.
        return match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).context("removing the autostart entry"),
        };
    }
    let exe = std::env::current_exe().context("locating the executable")?;
    // Quoted, because the path may contain spaces and Exec is parsed like a shell line.
    let entry = format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Lathe\n\
         Comment=Push-to-talk dictation\n\
         Exec=\"{}\"\n\
         Terminal=false\n\
         X-GNOME-Autostart-enabled=true\n",
        exe.display()
    );
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).context("creating the autostart folder")?;
    }
    std::fs::write(&path, entry).context("writing the autostart entry")
}
