//! Registry of engine processes, so that engines left behind by a crash or a
//! forced quit are stopped at the next start.
//!
//! Each running engine is written to a small text file as one line:
//! `app-pid TAB engine-pid TAB binary`. The app's own process ID is part of the
//! line, so a second copy of the app that is still running keeps its engines:
//! a sweep only touches lines whose app is gone. At start-up, [`sweep`] kills
//! the engine's process tree if it still runs the recorded binary, and drops
//! the line either way.
//!
//! "Process tree" means a Unix process group or a Windows process tree
//! (`taskkill /T`). The engine is spawned so that any helper it starts belongs
//! to it on both platforms.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Where the registry is kept, once the app has configured it. `None` (the
/// tests' default) makes every call a no-op.
static REGISTRY_FILE: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Use `path` for the registry from now on.
pub fn configure(path: PathBuf) {
    *REGISTRY_FILE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(path);
}

/// Record a running engine. On Unix `engine_pid` is also its process group ID;
/// on Windows the kill walks the process tree from the same ID.
pub fn register(engine_pid: u32, binary: &Path) {
    let Some(path) = configured() else { return };
    let _guard = lock();
    let mut lines = read_lines(&path);
    lines.push(format_line(std::process::id(), engine_pid, binary));
    write_lines(&path, &lines);
}

/// Forget an engine that has exited.
pub fn unregister(engine_pid: u32) {
    let Some(path) = configured() else { return };
    let _guard = lock();
    let app = std::process::id();
    let lines: Vec<String> = read_lines(&path)
        .into_iter()
        .filter(|line| {
            parse_line(line).is_none_or(|entry| entry.app != app || entry.engine != engine_pid)
        })
        .collect();
    write_lines(&path, &lines);
}

/// Stop the engines that an earlier run left behind. Lines whose app is still
/// running (this one included) are kept. Returns one message per engine stopped.
pub fn sweep_configured() -> Vec<String> {
    match configured() {
        Some(path) => {
            let _guard = lock();
            sweep(&path)
        }
        None => Vec::new(),
    }
}

/// [`sweep_configured`] for an explicit file.
pub fn sweep(path: &Path) -> Vec<String> {
    let mut messages = Vec::new();
    let mut kept = Vec::new();
    for line in read_lines(path) {
        let Some(entry) = parse_line(&line) else {
            continue;
        };
        if process_alive(entry.app) {
            kept.push(line);
            continue;
        }
        if process_alive(entry.engine)
            && command_of(entry.engine).is_some_and(|cmd| same_binary(&cmd, &entry.binary))
        {
            kill_group(entry.engine);
            messages.push(format!(
                "stopped an engine left running by an earlier session: {} (pid {})",
                entry.binary, entry.engine
            ));
        }
    }
    write_lines(path, &kept);
    messages
}

/// Stop the process tree led by `pid`. Errors are ignored: the process may
/// already be gone.
///
/// Unix sends `SIGKILL` to the process group. Windows asks `taskkill` to end
/// the process and its children (`/T`), which is the equivalent of the group
/// kill and also reaches a helper an engine started.
pub fn kill_group(pid: u32) {
    #[cfg(unix)]
    {
        // SAFETY: `killpg` only sends a signal; it reads no memory of ours.
        unsafe {
            libc::killpg(pid as libc::pid_t, libc::SIGKILL);
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use std::process::Stdio;

        let _ = std::process::Command::new(
            crate::platform::windows_dir()
                .join("System32")
                .join("taskkill.exe"),
        )
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .creation_flags(crate::platform::CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    }
}

fn configured() -> Option<PathBuf> {
    REGISTRY_FILE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

/// Serialises read-modify-write cycles within this process.
fn lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// One registry line.
struct Entry {
    app: u32,
    engine: u32,
    binary: String,
}

fn format_line(app: u32, engine: u32, binary: &Path) -> String {
    format!("{app}\t{engine}\t{}", binary.display())
}

fn parse_line(line: &str) -> Option<Entry> {
    let mut parts = line.splitn(3, '\t');
    Some(Entry {
        app: parts.next()?.parse().ok()?,
        engine: parts.next()?.parse().ok()?,
        binary: parts.next()?.to_string(),
    })
}

fn read_lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .map(|text| {
            text.lines()
                .filter(|line| !line.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn write_lines(path: &Path, lines: &[String]) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if lines.is_empty() {
        let _ = std::fs::remove_file(path);
        return;
    }
    let mut text = String::new();
    for line in lines {
        text.push_str(line);
        text.push('\n');
    }
    // Write beside the file and rename, so a crash cannot leave half a file.
    let tmp = path.with_extension("tmp");
    let written = std::fs::File::create(&tmp).and_then(|mut file| {
        file.write_all(text.as_bytes())?;
        file.sync_all()
    });
    if written.is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

/// `true` when a process with this ID exists.
fn process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // SAFETY: signal 0 only checks that the process exists.
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
        use windows_sys::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };

        // SAFETY: the handle is opened for a query only and closed before
        // returning; the out-parameter points at a live local variable.
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                return false;
            }
            let mut code = 0u32;
            let ok = GetExitCodeProcess(handle, &mut code);
            CloseHandle(handle);
            ok != 0 && code == STILL_ACTIVE as u32
        }
    }
}

/// The command line (Unix) or full image path (Windows) of a process, when the
/// system can tell. Used to make sure a recorded PID still belongs to the
/// engine it was recorded for before killing it.
fn command_of(pid: u32) -> Option<String> {
    #[cfg(unix)]
    {
        let output = std::process::Command::new("ps")
            .args(["-o", "command=", "-p", &pid.to_string()])
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
        (!text.is_empty()).then_some(text)
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{
            OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
        };

        // SAFETY: the buffer is a live local array and its length is passed
        // in and out; the process handle is closed before returning.
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                return None;
            }
            let mut buffer = [0u16; 32_768];
            let mut len = buffer.len() as u32;
            let ok = QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &mut len);
            CloseHandle(handle);
            if ok == 0 {
                return None;
            }
            let text = String::from_utf16_lossy(&buffer[..len as usize]);
            (!text.is_empty()).then_some(text)
        }
    }
}

/// Whether a process description names `binary`. Windows paths are compared
/// case-insensitively and with either slash, because the image path that the
/// system reports and the path the settings stored can differ in both.
fn same_binary(command: &str, binary: &str) -> bool {
    #[cfg(windows)]
    {
        let normalize = |text: &str| text.replace('/', "\\").to_lowercase();
        normalize(command).contains(&normalize(binary))
    }
    #[cfg(not(windows))]
    {
        command.contains(binary)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_round_trips_and_a_bad_line_is_ignored() {
        let line = format_line(12, 34, Path::new("/opt/engines/sf stock"));
        let entry = parse_line(&line).unwrap();
        assert_eq!(
            (entry.app, entry.engine, entry.binary.as_str()),
            (12, 34, "/opt/engines/sf stock")
        );
        assert!(parse_line("not a registry line").is_none());
    }

    #[test]
    fn a_stale_line_of_a_dead_app_is_dropped_and_the_file_removed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("engines.txt");
        // Neither process exists: the app pid is far above any real one.
        std::fs::write(
            &path,
            format_line(4_000_000, 4_000_001, Path::new("/nowhere/engine")) + "\n",
        )
        .unwrap();
        let messages = sweep(&path);
        assert!(
            messages.is_empty(),
            "nothing is running, so nothing is stopped"
        );
        assert!(!path.exists(), "the stale line is removed");
    }

    #[test]
    fn a_running_app_keeps_its_engine_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("engines.txt");
        let line = format_line(std::process::id(), 4_000_002, Path::new("/nowhere/engine"));
        std::fs::write(&path, format!("{line}\n")).unwrap();
        assert!(sweep(&path).is_empty());
        assert_eq!(read_lines(&path), vec![line], "a live app's line is kept");
    }

    #[cfg(unix)]
    #[test]
    fn a_helper_of_a_dead_app_is_stopped_with_its_engine() {
        use std::os::unix::fs::PermissionsExt;
        use std::os::unix::process::CommandExt;
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("orphan-engine.sh");
        std::fs::write(&script, "#!/bin/sh\nsleep 4242\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let child = std::process::Command::new(&script)
            .process_group(0)
            .spawn()
            .unwrap();
        let engine = child.id();
        let path = dir.path().join("engines.txt");
        std::fs::write(
            &path,
            format!("{}\n", format_line(4_000_003, engine, &script)),
        )
        .unwrap();

        let messages = sweep(&path);
        assert_eq!(
            messages.len(),
            1,
            "the engine of a dead app is stopped: {messages:?}"
        );
        assert!(!path.exists());
        let mut child = child;
        let _ = child.wait();
        assert!(!process_alive(engine), "the group is gone");
    }

    #[cfg(windows)]
    #[test]
    fn a_process_of_a_dead_app_is_stopped_by_the_registry() {
        use std::os::windows::process::CommandExt;
        use std::process::Stdio;

        // A long-running system program stands in for an engine: it must be
        // found alive, matched by its image path, and killed after its app is
        // recorded as gone.
        let ping = crate::platform::windows_dir()
            .join("System32")
            .join("ping.exe");
        let mut child = std::process::Command::new(&ping)
            .args(["-n", "30", "127.0.0.1"])
            .creation_flags(crate::platform::CREATE_NO_WINDOW)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let engine = child.id();
        assert!(process_alive(engine), "the stand-in engine runs");

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("engines.txt");
        std::fs::write(
            &path,
            format!("{}\n", format_line(4_000_004, engine, &ping)),
        )
        .unwrap();

        let messages = sweep(&path);
        assert_eq!(
            messages.len(),
            1,
            "the engine of a dead app is stopped: {messages:?}"
        );
        assert!(!path.exists(), "the stale line is removed");
        let _ = child.wait();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while process_alive(engine) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(!process_alive(engine), "the process is gone");
    }
}
