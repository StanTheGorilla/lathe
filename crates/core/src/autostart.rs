// Launch at login. Brief section 5.7.
//
// An HKCU Run entry rather than a scheduled task or a Startup-folder shortcut: it needs
// no elevation, it is what the Settings app's startup list reads, and removing it is a
// single registry delete rather than a file the user has to find.

use anyhow::{Context as _, Result};
use windows::core::{w, HSTRING};
use windows::Win32::Foundation::ERROR_FILE_NOT_FOUND;
use windows::Win32::System::Registry::{
    RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY,
    HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_SZ,
};

const VALUE_NAME: windows::core::PCWSTR = w!("Lathe");

fn open(access: windows::Win32::System::Registry::REG_SAM_FLAGS) -> Result<HKEY> {
    let mut key = HKEY::default();
    unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Run"),
            None,
            access,
            &mut key,
        )
        .ok()
        .context("opening the Run key")?;
    }
    Ok(key)
}

pub fn is_enabled() -> bool {
    let Ok(key) = open(KEY_READ) else {
        return false;
    };
    let mut size = 0u32;
    let result = unsafe { RegQueryValueExW(key, VALUE_NAME, None, None, None, Some(&mut size)) };
    unsafe { let _ = RegCloseKey(key); }
    result.is_ok()
}

pub fn set_enabled(enabled: bool) -> Result<()> {
    let key = open(KEY_WRITE)?;
    let result = if enabled {
        let exe = std::env::current_exe().context("locating the executable")?;
        // Quoted, because the path may contain spaces and the Run key hands the value
        // straight to the shell.
        let command = HSTRING::from(format!("\"{}\"", exe.display()));
        let bytes: &[u8] = unsafe {
            std::slice::from_raw_parts(
                command.as_ptr() as *const u8,
                (command.len() + 1) * 2,
            )
        };
        unsafe { RegSetValueExW(key, VALUE_NAME, None, REG_SZ, Some(bytes)) }
    } else {
        let deleted = unsafe { RegDeleteValueW(key, VALUE_NAME) };
        // Absent is the desired state, so a missing value is success, not failure.
        if deleted == ERROR_FILE_NOT_FOUND {
            windows::Win32::Foundation::WIN32_ERROR(0)
        } else {
            deleted
        }
    };
    unsafe { let _ = RegCloseKey(key); }
    result.ok().context("writing the Run key")?;
    Ok(())
}
