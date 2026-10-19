//! The Windows parts of installing: shortcuts, the entry in Settings > Apps, start with Windows,
//! and running programs without a console window. Elsewhere these do nothing (the installer is
//! for Windows; this keeps it compiling for tests).

use std::path::{Path, PathBuf};
use std::time::Duration;

#[cfg(windows)]
pub use imp::*;

#[cfg(not(windows))]
pub use other::*;

/// Runs `exe` with `args` without a window, waiting up to `limit`; true if it finished and
/// succeeded.
pub fn run_hidden(exe: &Path, args: &[&str], limit: Duration) -> bool {
    let mut cmd = std::process::Command::new(exe);
    cmd.args(args)
        .current_dir(exe.parent().unwrap_or(Path::new(".")))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let Ok(mut child) = cmd.spawn() else {
        return false;
    };
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if start.elapsed() < limit => std::thread::sleep(Duration::from_millis(100)),
            _ => {
                let _ = child.kill();
                return false;
            }
        }
    }
}

/// Starts `exe` on its own (it keeps running after the installer closes).
pub fn launch(exe: &Path, args: &[&str]) -> Result<(), String> {
    std::process::Command::new(exe)
        .args(args)
        .current_dir(exe.parent().unwrap_or(Path::new(".")))
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("Couldn't start {}: {e}", exe.display()))
}

/// The Start menu's Programs folder for this user.
pub fn start_menu_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|d| {
        d.join("Microsoft")
            .join("Windows")
            .join("Start Menu")
            .join("Programs")
    })
}

#[cfg(windows)]
mod imp {
    use std::path::{Path, PathBuf};

    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
        IPersistFile,
    };
    use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
    use windows_core::{HSTRING, Interface};
    use windows_sys::Win32::Security::Credentials::{CRED_TYPE_GENERIC, CredDeleteW};
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_DWORD, REG_OPTION_NON_VOLATILE, REG_SZ,
        RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegDeleteValueW, RegOpenKeyExW,
        RegQueryValueExW, RegSetValueExW,
    };

    const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Kairo";
    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const STARTUP_APPROVED: &str =
        r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    /// Makes (or replaces) a shortcut at `lnk` that starts `target` in `dir`.
    pub fn shortcut(lnk: &Path, target: &Path, dir: &Path) -> Result<(), String> {
        if let Some(p) = lnk.parent() {
            let _ = std::fs::create_dir_all(p);
        }
        // SAFETY: COM calls on this thread, which joins a single-threaded apartment first (a
        // second join on the same thread is harmless).
        let r = unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            (|| -> windows_core::Result<()> {
                let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
                link.SetPath(&HSTRING::from(target))?;
                link.SetWorkingDirectory(&HSTRING::from(dir))?;
                link.SetDescription(&HSTRING::from("Instant replay: save the last moments"))?;
                link.SetIconLocation(&HSTRING::from(target), 0)?;
                let file: IPersistFile = link.cast()?;
                file.Save(&HSTRING::from(lnk), true)
            })()
        };
        r.map_err(|e| format!("Couldn't make the shortcut {}: {e}", lnk.display()))
    }

    enum Value<'a> {
        Text(&'a str),
        Number(u32),
    }

    /// Adds Kairo to Settings > Apps (and Control Panel's Programs and Features).
    pub fn register(dir: &Path, version: &str, size_kb: u32) -> Result<(), String> {
        let exe = dir.join(crate::install::PROGRAM);
        let uninstaller = dir.join(crate::install::UNINSTALLER);
        let today = {
            // YYYYMMDD from the system clock, without a date library.
            let days = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs() / 86_400) as i64;
            let (y, m, d) = civil_from_days(days);
            format!("{y:04}{m:02}{d:02}")
        };
        let icon = format!("{},0", exe.display());
        let location = dir.display().to_string();
        let uninstall = format!("\"{}\" --uninstall", uninstaller.display());
        let quiet = format!("{uninstall} --silent");
        let values = [
            ("DisplayName", Value::Text("Kairo")),
            ("DisplayVersion", Value::Text(version)),
            ("Publisher", Value::Text("Kairo")),
            ("DisplayIcon", Value::Text(&icon)),
            ("InstallLocation", Value::Text(&location)),
            ("UninstallString", Value::Text(&uninstall)),
            ("QuietUninstallString", Value::Text(&quiet)),
            (
                "URLInfoAbout",
                Value::Text("https://github.com/fsiuhgshiqoih32/kairo"),
            ),
            ("InstallDate", Value::Text(&today)),
            ("EstimatedSize", Value::Number(size_kb)),
            ("NoModify", Value::Number(1)),
            ("NoRepair", Value::Number(1)),
        ];
        let mut key: HKEY = std::ptr::null_mut();
        let path = wide(UNINSTALL_KEY);
        // SAFETY: plain registry calls with NUL-terminated names; the key is closed below.
        unsafe {
            let e = RegCreateKeyExW(
                HKEY_CURRENT_USER,
                path.as_ptr(),
                0,
                std::ptr::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_WRITE,
                std::ptr::null(),
                &mut key,
                std::ptr::null_mut(),
            );
            if e != 0 {
                return Err(format!(
                    "Couldn't add Kairo to Settings > Apps (error {e})."
                ));
            }
            for (name, value) in &values {
                let name = wide(name);
                match value {
                    Value::Text(t) => {
                        let data = wide(t);
                        RegSetValueExW(
                            key,
                            name.as_ptr(),
                            0,
                            REG_SZ,
                            data.as_ptr() as *const u8,
                            (data.len() * 2) as u32,
                        );
                    }
                    Value::Number(n) => {
                        RegSetValueExW(
                            key,
                            name.as_ptr(),
                            0,
                            REG_DWORD,
                            n.to_le_bytes().as_ptr(),
                            4,
                        );
                    }
                }
            }
            RegCloseKey(key);
        }
        Ok(())
    }

    /// Removes Kairo from Settings > Apps.
    pub fn unregister() {
        let path = wide(UNINSTALL_KEY);
        // SAFETY: deletes the key and its values.
        unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, path.as_ptr()) };
        let parent = wide(r"Software\Microsoft\Windows\CurrentVersion\Uninstall");
        let mut key: HKEY = std::ptr::null_mut();
        // SAFETY: opens the parent to delete the (now empty) key itself.
        unsafe {
            if RegOpenKeyExW(HKEY_CURRENT_USER, parent.as_ptr(), 0, KEY_WRITE, &mut key) == 0 {
                let name = wide("Kairo");
                windows_sys::Win32::System::Registry::RegDeleteKeyW(key, name.as_ptr());
                RegCloseKey(key);
            }
        }
    }

    /// A text value under HKEY_CURRENT_USER, if it's there.
    fn read_text(key_path: &str, name: &str) -> Option<String> {
        let path = wide(key_path);
        let name = wide(name);
        let mut key: HKEY = std::ptr::null_mut();
        // SAFETY: reads into a buffer of the size the first call reports; the key is closed.
        unsafe {
            if RegOpenKeyExW(HKEY_CURRENT_USER, path.as_ptr(), 0, KEY_READ, &mut key) != 0 {
                return None;
            }
            let mut len = 0u32;
            let mut kind = 0u32;
            let mut out = None;
            if RegQueryValueExW(
                key,
                name.as_ptr(),
                std::ptr::null(),
                &mut kind,
                std::ptr::null_mut(),
                &mut len,
            ) == 0
                && kind == REG_SZ
            {
                let mut buf = vec![0u16; (len as usize).div_ceil(2)];
                if RegQueryValueExW(
                    key,
                    name.as_ptr(),
                    std::ptr::null(),
                    &mut kind,
                    buf.as_mut_ptr() as *mut u8,
                    &mut len,
                ) == 0
                {
                    let end = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
                    out = Some(String::from_utf16_lossy(&buf[..end]));
                }
            }
            RegCloseKey(key);
            out
        }
    }

    /// Where Kairo is installed, per its entry in Settings > Apps.
    pub fn installed_dir() -> Option<PathBuf> {
        read_text(UNINSTALL_KEY, "InstallLocation")
            .map(PathBuf::from)
            .filter(|d| d.join(crate::install::PROGRAM).exists())
    }

    /// The version installed, per its entry in Settings > Apps.
    pub fn installed_version() -> Option<String> {
        installed_dir()?;
        read_text(UNINSTALL_KEY, "DisplayVersion")
    }

    /// Whether Kairo starts with Windows now.
    pub fn starts_with_windows() -> bool {
        read_text(RUN_KEY, "kairo").is_some()
    }

    /// The program a start-up entry runs (entries hold `<program> daemon`).
    fn start_entry_program(name: &str) -> Option<PathBuf> {
        let v = read_text(RUN_KEY, name)?;
        let v = v.trim().trim_start_matches('"');
        let end = v.to_ascii_lowercase().find(".exe")? + 4;
        Some(PathBuf::from(&v[..end]))
    }

    /// The folder of a Rewindr (Kairo's name before 0.2.0) that starts with Windows.
    pub fn old_rewindr_dir() -> Option<PathBuf> {
        ["rewindr", "kairo"].iter().find_map(|name| {
            let exe = start_entry_program(name)?;
            let old = exe
                .file_name()?
                .eq_ignore_ascii_case("rewindr.exe")
                .then(|| exe.parent().map(Path::to_path_buf))??;
            exe.exists().then_some(old)
        })
    }

    /// Takes out of start-up the Rewindr or Kairo entries that run a program inside `dir`.
    pub fn remove_start_entries_into(dir: &Path) {
        let lower = |p: &Path| PathBuf::from(p.to_string_lossy().to_lowercase());
        for name in ["rewindr", "kairo"] {
            if start_entry_program(name).is_some_and(|exe| lower(&exe).starts_with(lower(dir))) {
                delete_value(RUN_KEY, name);
                delete_value(STARTUP_APPROVED, name);
            }
        }
    }

    fn delete_value(key_path: &str, name: &str) {
        let path = wide(key_path);
        let name = wide(name);
        let mut key: HKEY = std::ptr::null_mut();
        // SAFETY: opens the key, deletes one value and closes it.
        unsafe {
            if RegOpenKeyExW(HKEY_CURRENT_USER, path.as_ptr(), 0, KEY_WRITE, &mut key) == 0 {
                RegDeleteValueW(key, name.as_ptr());
                RegCloseKey(key);
            }
        }
    }

    /// Forgets the Discord channel Kairo kept in Credential Manager.
    pub fn forget_secrets() {
        let target = wide("Kairo/discord-webhook");
        // SAFETY: a plain call with a NUL-terminated name.
        unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) };
    }

    /// Deletes `file` a few seconds after this program exits (a running program can't delete
    /// itself).
    pub fn delete_later(file: &Path) {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let _ = std::process::Command::new("cmd.exe")
            .raw_arg(format!(
                "/d /c ping -n 4 127.0.0.1 >nul & del /f /q \"{}\"",
                file.display()
            ))
            .creation_flags(CREATE_NO_WINDOW)
            .spawn();
    }

    /// Year, month, day of a day count since 1970-01-01 (Howard Hinnant's algorithm).
    fn civil_from_days(z: i64) -> (i64, u32, u32) {
        let z = z + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z.rem_euclid(146_097);
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
        let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
        (if m <= 2 { y + 1 } else { y }, m, d)
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn dates_from_day_counts() {
            assert_eq!(super::civil_from_days(0), (1970, 1, 1));
            assert_eq!(super::civil_from_days(20_745), (2026, 10, 19));
            assert_eq!(super::civil_from_days(11_016), (2000, 2, 29));
        }
    }
}

#[cfg(not(windows))]
mod other {
    use std::path::{Path, PathBuf};

    pub fn shortcut(_: &Path, _: &Path, _: &Path) -> Result<(), String> {
        Ok(())
    }
    pub fn register(_: &Path, _: &str, _: u32) -> Result<(), String> {
        Ok(())
    }
    pub fn unregister() {}
    pub fn installed_dir() -> Option<PathBuf> {
        None
    }
    pub fn installed_version() -> Option<String> {
        None
    }
    pub fn starts_with_windows() -> bool {
        false
    }
    pub fn forget_secrets() {}
    pub fn delete_later(_: &Path) {}
    pub fn old_rewindr_dir() -> Option<PathBuf> {
        None
    }
    pub fn remove_start_entries_into(_: &Path) {}
}
