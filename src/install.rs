// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvsnip contributors
// Per-user install, shared with the setup program (installer/), so it only uses winreg and windows-sys.
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE};
use winreg::RegKey;

pub const APP_NAME: &str = "dvsnip";
pub const EXE_NAME: &str = "dvsnip.exe";
pub const CLI_EXE_NAME: &str = "dvsnip-cli.exe";
/// the hidden window of the running instance; other processes find it to hand over a command
pub const RESIDENT_CLASS: &str = "dvsnipResident";
const PROG_ID: &str = "dvsnip.Url";
const PROTOCOLS: [&str; 2] = ["ms-screenclip", "ms-screensketch"];
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const KEYBOARD_KEY: &str = r"Control Panel\Keyboard";
const PRTSC_VALUE: &str = "PrintScreenKeyForSnippingEnabled";
const BACKUP_VALUE: &str = "WindowsPrintScreen";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub fn install_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("Programs").join(APP_NAME)
}

fn shortcut_path() -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    Some(PathBuf::from(appdata).join(r"Microsoft\Windows\Start Menu\Programs").join(format!("{APP_NAME}.lnk")))
}

fn hkcu() -> RegKey {
    RegKey::predef(HKEY_CURRENT_USER)
}

pub fn installed_exe() -> Option<PathBuf> {
    let key = hkcu()
        .open_subkey_with_flags(format!(r"Software\Microsoft\Windows\CurrentVersion\Uninstall\{APP_NAME}"), KEY_READ)
        .ok()?;
    let exe: String = key.get_value("DisplayIcon").ok()?;
    let exe = PathBuf::from(exe);
    exe.is_file().then_some(exe)
}

pub fn is_installed() -> bool {
    installed_exe().is_some()
}

#[allow(dead_code)] // used by the installer, which shares this file
pub fn installed_version() -> Option<String> {
    hkcu()
        .open_subkey_with_flags(format!(r"Software\Microsoft\Windows\CurrentVersion\Uninstall\{APP_NAME}"), KEY_READ)
        .ok()?
        .get_value("DisplayVersion")
        .ok()
}

#[allow(dead_code)]
pub fn is_newer(a: &str, b: &str) -> bool {
    let parts = |v: &str| v.split('.').map(|p| p.parse::<u32>().unwrap_or(0)).collect::<Vec<_>>();
    parts(a) > parts(b)
}

// A running exe cannot be overwritten but it can be renamed, so the old file is moved aside and the new one written in
// its place. The old copy is deleted right away unless it is still running; the next update removes it then.
pub fn replace_exe(target: &Path, write: impl FnOnce(&Path) -> std::io::Result<()>) -> Result<(), String> {
    let old = target.with_extension("old");
    let _ = std::fs::remove_file(&old);
    let moved = target.exists();
    if moved {
        std::fs::rename(target, &old).map_err(|e| format!("Cannot replace {}: {e}", target.display()))?;
    }
    if let Err(e) = write(target) {
        if moved {
            let _ = std::fs::rename(&old, target);
        }
        return Err(format!("Cannot write {}: {e}", target.display()));
    }
    let _ = std::fs::remove_file(&old);
    Ok(())
}

#[allow(dead_code)] // the installer embeds its own copy instead
pub fn install() -> Result<PathBuf, String> {
    let current = std::env::current_exe().map_err(|e| e.to_string())?;
    let dir = install_dir();
    let target = dir.join(EXE_NAME);
    let same = std::fs::canonicalize(&current).ok() == std::fs::canonicalize(&target).ok();
    if !same {
        std::fs::create_dir_all(&dir).map_err(|e| format!("Cannot create {}: {e}", dir.display()))?;
        replace_exe(&target, |t| std::fs::copy(&current, t).map(|_| ()))?;
        // the console tool, when it sits next to this exe (as it does in the build folder)
        if let Some(cli) = current.parent().map(|d| d.join(CLI_EXE_NAME)).filter(|p| p.is_file()) {
            replace_exe(&dir.join(CLI_EXE_NAME), |t| std::fs::copy(&cli, t).map(|_| ()))?;
        }
    }
    finish_install(&target)?;
    Ok(target)
}

pub fn finish_install(exe: &Path) -> Result<(), String> {
    register(exe).map_err(|e| format!("Registry write failed: {e}"))?;
    set_start_with_windows(true, Some(exe));
    take_print_screen();
    create_shortcut(exe);
    if let Some(dir) = exe.parent() {
        set_on_path(dir, true);
    }
    Ok(())
}

const ENV_KEY: &str = "Environment";

/// Adds `dir` to (or removes it from) the user's Path, so `dvsnip-cli` runs from any terminal or agent.
pub fn set_on_path(dir: &Path, on: bool) {
    use winreg::enums::REG_EXPAND_SZ;
    let Ok(key) = hkcu().open_subkey_with_flags(ENV_KEY, KEY_READ | KEY_SET_VALUE) else { return };
    let current: String = key.get_value("Path").unwrap_or_default();
    let want = dir.display().to_string();
    let same = |p: &str| p.trim_end_matches('\\').eq_ignore_ascii_case(want.trim_end_matches('\\'));
    let mut parts: Vec<&str> = current.split(';').filter(|p| !p.is_empty()).collect();
    let present = parts.iter().any(|p| same(p));
    if on == present {
        return;
    }
    if on {
        parts.push(&want);
    } else {
        parts.retain(|p| !same(p));
    }
    let joined = parts.join(";");
    let bytes: Vec<u8> = joined.encode_utf16().chain(Some(0)).flat_map(u16::to_le_bytes).collect();
    let _ = key.set_raw_value("Path", &winreg::RegValue { vtype: REG_EXPAND_SZ, bytes: bytes.into() });
    broadcast_environment();
}

fn broadcast_environment() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SendMessageTimeoutW, HWND_BROADCAST, SMTO_ABORTIFHUNG, WM_SETTINGCHANGE,
    };
    let env = wide("Environment");
    let mut result = 0;
    unsafe {
        SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            0,
            env.as_ptr() as isize,
            SMTO_ABORTIFHUNG,
            2000,
            &mut result,
        );
    }
}

fn register(exe: &Path) -> std::io::Result<()> {
    let exe = exe.display().to_string();
    let command = format!("\"{exe}\" \"%1\"");
    let root = hkcu();

    // a URL handler Windows can offer under Default apps > "Choose defaults by link type"
    let (class, _) = root.create_subkey(format!(r"Software\Classes\{PROG_ID}"))?;
    class.set_value("", &"dvsnip screen capture")?;
    class.set_value("URL Protocol", &"")?;
    class.create_subkey("DefaultIcon")?.0.set_value("", &format!("\"{exe}\",0"))?;
    class.create_subkey(r"shell\open\command")?.0.set_value("", &command)?;

    let (caps, _) = root.create_subkey(format!(r"Software\{APP_NAME}\Capabilities"))?;
    caps.set_value("ApplicationName", &APP_NAME)?;
    caps.set_value("ApplicationDescription", &"Screen capture: snip, mark up, copy and save")?;
    let (urls, _) = caps.create_subkey("URLAssociations")?;
    for p in PROTOCOLS {
        urls.set_value(p, &PROG_ID)?;
    }
    root.create_subkey(r"Software\RegisteredApplications")?
        .0
        .set_value(APP_NAME, &format!(r"Software\{APP_NAME}\Capabilities"))?;

    let (un, _) = root.create_subkey(format!(r"Software\Microsoft\Windows\CurrentVersion\Uninstall\{APP_NAME}"))?;
    un.set_value("DisplayName", &APP_NAME)?;
    un.set_value("DisplayVersion", &env!("CARGO_PKG_VERSION"))?;
    un.set_value("Publisher", &APP_NAME)?;
    un.set_value("DisplayIcon", &exe)?;
    un.set_value("InstallLocation", &install_dir().display().to_string())?;
    un.set_value("UninstallString", &format!("\"{exe}\" --uninstall"))?;
    un.set_value("NoModify", &1u32)?;
    un.set_value("NoRepair", &1u32)?;
    Ok(())
}

pub fn start_with_windows() -> bool {
    hkcu().open_subkey_with_flags(RUN_KEY, KEY_READ).and_then(|k| k.get_value::<String, _>(APP_NAME)).is_ok()
}

/// Adds or removes the sign-in entry that starts dvsnip in the background.
pub fn set_start_with_windows(on: bool, exe: Option<&Path>) {
    let Ok((key, _)) = hkcu().create_subkey(RUN_KEY) else { return };
    if on {
        let exe = exe.map(Path::to_path_buf).or_else(installed_exe).or_else(|| std::env::current_exe().ok());
        if let Some(exe) = exe {
            let _ = key.set_value(APP_NAME, &format!("\"{}\" --background", exe.display()));
        }
    } else {
        let _ = key.delete_value(APP_NAME);
    }
}

/// Whether Windows itself answers Print Screen with its screen capture. Windows 11 does so unless the value is 0.
pub fn windows_takes_print_screen() -> bool {
    hkcu()
        .open_subkey_with_flags(KEYBOARD_KEY, KEY_READ)
        .and_then(|k| k.get_value::<u32, _>(PRTSC_VALUE))
        .map_or(true, |v| v != 0)
}

/// Turns off "Use the Print screen key to open screen capture" so Print Screen reaches dvsnip as a plain hotkey.
/// The previous value is kept (once) and put back by `restore_print_screen`.
pub fn take_print_screen() {
    let root = hkcu();
    let Ok(kb) = root.open_subkey_with_flags(KEYBOARD_KEY, KEY_READ | KEY_SET_VALUE) else { return };
    if let Ok((app, _)) = root.create_subkey(format!(r"Software\{APP_NAME}")) {
        if app.get_raw_value(BACKUP_VALUE).is_err() {
            let _ = match kb.get_value::<u32, _>(PRTSC_VALUE) {
                Ok(v) => app.set_value(BACKUP_VALUE, &v),
                Err(_) => app.set_value(BACKUP_VALUE, &"absent"),
            };
        }
    }
    let _ = kb.set_value(PRTSC_VALUE, &0u32);
}

pub fn restore_print_screen() {
    let root = hkcu();
    let Ok(app) = root.open_subkey_with_flags(format!(r"Software\{APP_NAME}"), KEY_READ | KEY_SET_VALUE) else {
        return;
    };
    let Ok(kb) = root.open_subkey_with_flags(KEYBOARD_KEY, KEY_SET_VALUE) else { return };
    if let Ok(v) = app.get_value::<u32, _>(BACKUP_VALUE) {
        let _ = kb.set_value(PRTSC_VALUE, &v);
    } else if app.get_value::<String, _>(BACKUP_VALUE).is_ok() {
        let _ = kb.delete_value(PRTSC_VALUE);
    } else {
        return;
    }
    let _ = app.delete_value(BACKUP_VALUE);
}

fn create_shortcut(exe: &Path) {
    let Some(lnk) = shortcut_path() else { return };
    let quote = |p: &str| p.replace('\'', "''");
    let script = format!(
        "$s=(New-Object -ComObject WScript.Shell).CreateShortcut('{}');$s.TargetPath='{}';$s.IconLocation='{},0';$s.Description='Snip the screen';$s.Save()",
        quote(&lnk.display().to_string()),
        quote(&exe.display().to_string()),
        quote(&exe.display().to_string())
    );
    let _ = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .creation_flags(CREATE_NO_WINDOW)
        .status();
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn resident_window() -> Option<windows_sys::Win32::Foundation::HWND> {
    use windows_sys::Win32::UI::WindowsAndMessaging::FindWindowW;
    let class = wide(RESIDENT_CLASS);
    let h = unsafe { FindWindowW(class.as_ptr(), std::ptr::null()) };
    (!h.is_null()).then_some(h)
}

/// Hands a command-line argument ("--quit", an ms-screenclip: link, "" for a snip) to the running instance.
/// Returns false when none is running.
pub fn send_to_resident(cmd: &str) -> bool {
    use windows_sys::Win32::System::DataExchange::COPYDATASTRUCT;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        AllowSetForegroundWindow, GetWindowThreadProcessId, SendMessageTimeoutW, SMTO_ABORTIFHUNG, WM_COPYDATA,
    };
    let Some(h) = resident_window() else { return false };
    unsafe {
        // this process was just started by the user, so it may pass its right to take the foreground along
        let mut pid = 0;
        GetWindowThreadProcessId(h, &mut pid);
        AllowSetForegroundWindow(pid);
        let data = COPYDATASTRUCT { dwData: 1, cbData: cmd.len() as u32, lpData: cmd.as_ptr() as *mut _ };
        let mut result = 0;
        SendMessageTimeoutW(h, WM_COPYDATA, 0, &data as *const _ as isize, SMTO_ABORTIFHUNG, 3000, &mut result) != 0
    }
}

/// Asks a running instance to exit and waits up to three seconds for it.
pub fn quit_resident() {
    if send_to_resident("--quit") {
        for _ in 0..60 {
            if resident_window().is_none() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
}

pub fn uninstall() -> Result<(), String> {
    quit_resident();
    restore_print_screen();
    set_start_with_windows(false, None);
    set_on_path(&install_dir(), false);
    let root = hkcu();
    for key in [
        format!(r"Software\Classes\{PROG_ID}"),
        format!(r"Software\{APP_NAME}"),
        format!(r"Software\Microsoft\Windows\CurrentVersion\Uninstall\{APP_NAME}"),
    ] {
        let _ = root.delete_subkey_all(key);
    }
    if let Ok(k) = root.open_subkey_with_flags(r"Software\RegisteredApplications", KEY_SET_VALUE) {
        let _ = k.delete_value(APP_NAME);
    }
    if let Some(lnk) = shortcut_path() {
        let _ = std::fs::remove_file(lnk);
    }

    let dir = install_dir();
    if dir.join(EXE_NAME).is_file() {
        let cmd = format!("/C ping -n 3 127.0.0.1 >nul & rmdir /s /q \"{}\"", dir.display());
        std::process::Command::new("cmd")
            // cmd.exe needs raw quotes
            .raw_arg(&cmd)
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map_err(|e| format!("Unregistered, but could not remove {}: {e}", dir.display()))?;
    }
    Ok(())
}
