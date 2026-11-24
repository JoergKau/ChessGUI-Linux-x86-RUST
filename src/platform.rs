//! Platform-specific locations and small OS helpers.
//!
//! Everything that has to know whether it runs on Windows, macOS or Linux
//! asks this module: the home folder, the settings and log folders, the
//! Documents folder, and the file manager. Keeping the `cfg` blocks in one
//! place lets the rest of the app read as if the platforms were the same.

use std::path::PathBuf;

/// `CreateProcess` flag: do not create a console window for a child process.
/// Engines are console programs; without this flag every engine would flash a
/// black window next to the GUI window.
#[cfg(windows)]
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// `CreateProcess` flag: make the child the root of its own process group.
#[cfg(windows)]
pub const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

/// The Windows folder, from `%SystemRoot%` (normally `C:\Windows`). Used to
/// start the system tools (`taskkill`, `explorer`, PowerShell) by their full
/// path instead of trusting the search order for a `PATH` that a desktop
/// launch may have shortened.
#[cfg(windows)]
pub fn windows_dir() -> PathBuf {
    std::env::var_os("SystemRoot")
        .filter(|value| !value.is_empty())
        .map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from)
}

/// A non-empty environment variable. Windows commonly defines variables as
/// empty strings, which must count as "not set" for a path lookup.
fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// An absolute directory from the environment, or `None`. Relative values
/// (and empty ones) are ignored: a base directory that is not absolute would
/// place the app's files somewhere unexpected.
#[cfg(all(not(target_os = "macos"), not(windows)))]
fn absolute_env(name: &str) -> Option<PathBuf> {
    env_path(name).filter(|path| path.is_absolute())
}

/// The user's home folder: `$HOME` on macOS and Linux, `%USERPROFILE%` on
/// Windows with the older `HOMEDRIVE`/`HOMEPATH` pair as a fallback.
pub fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        if let Some(profile) = env_path("USERPROFILE") {
            return Some(profile);
        }
        match (std::env::var_os("HOMEDRIVE"), std::env::var_os("HOMEPATH")) {
            (Some(drive), Some(path)) if !drive.is_empty() && !path.is_empty() => {
                let mut joined = drive;
                joined.push(path);
                Some(PathBuf::from(joined))
            }
            _ => None,
        }
    }
    #[cfg(not(windows))]
    {
        env_path("HOME")
    }
}

/// The per-user configuration folder for the app:
///
/// - macOS: `~/Library/Application Support/SiliconBoard`
/// - Linux and other Unixes: `~/.config/siliconboard`, honouring
///   `XDG_CONFIG_HOME`
/// - Windows: `%APPDATA%\SiliconBoard`
pub fn config_dir() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        home_dir().map(|home| home.join("Library/Application Support/SiliconBoard"))
    }
    #[cfg(all(not(target_os = "macos"), not(windows)))]
    {
        if let Some(xdg) = absolute_env("XDG_CONFIG_HOME") {
            return Some(xdg.join("siliconboard"));
        }
        home_dir().map(|home| home.join(".config").join("siliconboard"))
    }
    #[cfg(windows)]
    {
        if let Some(appdata) = env_path("APPDATA") {
            return Some(appdata.join("SiliconBoard"));
        }
        home_dir().map(|home| home.join("AppData").join("Roaming").join("SiliconBoard"))
    }
}

/// The per-user session-log folder:
///
/// - macOS: `~/Library/Logs/SiliconBoard`
/// - Linux and other Unixes: `~/.local/state/siliconboard/logs`, honouring
///   `XDG_STATE_HOME`
/// - Windows: `%LOCALAPPDATA%\SiliconBoard\logs`
pub fn log_dir() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        home_dir().map(|home| home.join("Library/Logs/SiliconBoard"))
    }
    #[cfg(all(not(target_os = "macos"), not(windows)))]
    {
        let state = absolute_env("XDG_STATE_HOME")
            .or_else(|| home_dir().map(|home| home.join(".local/state")))?;
        Some(state.join("siliconboard/logs"))
    }
    #[cfg(windows)]
    {
        if let Some(local) = env_path("LOCALAPPDATA") {
            return Some(local.join("SiliconBoard").join("logs"));
        }
        home_dir().map(|home| {
            home.join("AppData")
                .join("Local")
                .join("SiliconBoard")
                .join("logs")
        })
    }
}

/// The user's Documents folder. On Windows this asks the shell for the real
/// known folder, so a redirected or OneDrive-synced Documents folder is
/// honoured instead of assuming `%USERPROFILE%\Documents`.
pub fn documents_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        if let Some(folder) = known_documents_dir() {
            return Some(folder);
        }
    }
    home_dir().map(|home| home.join("Documents"))
}

/// The Windows known-folder lookup behind [`documents_dir`].
#[cfg(windows)]
fn known_documents_dir() -> Option<PathBuf> {
    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::UI::Shell::{FOLDERID_Documents, SHGetKnownFolderPath};

    let mut raw: *mut u16 = std::ptr::null_mut();
    // SAFETY: `raw` is a valid out-parameter; Windows allocates the string
    // with CoTaskMemAlloc, which is released with CoTaskMemFree below.
    let result =
        unsafe { SHGetKnownFolderPath(&FOLDERID_Documents, 0, std::ptr::null_mut(), &mut raw) };
    if result < 0 || raw.is_null() {
        return None;
    }
    let len = unsafe {
        // The returned pointer is NUL-terminated; `raw` is non-null.
        let mut len = 0usize;
        while *raw.add(len) != 0 {
            len += 1;
        }
        len
    };
    let path = unsafe { String::from_utf16_lossy(std::slice::from_raw_parts(raw, len)) };
    unsafe { CoTaskMemFree(raw.cast()) };
    (!path.is_empty()).then(|| PathBuf::from(path))
}

/// Reveal `path` in the platform's file manager.
///
/// macOS selects the file in Finder, Windows selects it in Explorer, and
/// Linux hands its containing folder to `xdg-open` (opening the file itself
/// would ask for an application that handles a private file type).
pub fn reveal_in_file_manager(path: &std::path::Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg("-R")
            .arg(path)
            .spawn()
            .map(|_| ())
            .map_err(|err| format!("Could not open Finder: {err}"))
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;

        // Explorer's `/select,` takes the path in the same argument. Raw
        // quoting keeps a path with spaces together (CommandLineToArgvW would
        // otherwise split `/select,C:\My Folder\file`).
        std::process::Command::new(windows_dir().join("explorer.exe"))
            .raw_arg(format!("/select,\"{}\"", path.display()))
            .spawn()
            .map(|_| ())
            .map_err(|err| format!("Could not open Explorer: {err}"))
    }
    #[cfg(all(not(target_os = "macos"), not(windows)))]
    {
        let folder = path.parent().unwrap_or(path);
        std::process::Command::new("xdg-open")
            .arg(folder)
            .spawn()
            .map(|_| ())
            .map_err(|err| format!("Could not open the file manager: {err}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_platform_folders_are_absolute_and_distinct() {
        for folder in [config_dir(), log_dir(), documents_dir(), home_dir()]
            .into_iter()
            .flatten()
        {
            assert!(
                folder.is_absolute(),
                "a per-user folder must be absolute: {folder:?}"
            );
        }
        if let (Some(config), Some(logs)) = (config_dir(), log_dir()) {
            assert_ne!(config, logs, "settings and logs live in different folders");
        }
    }
}
