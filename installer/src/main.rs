// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvsnip contributors
#![windows_subsystem = "windows"]

#[allow(dead_code)]
#[path = "../../src/install.rs"]
mod install;

use windows_sys::Win32::UI::WindowsAndMessaging::{
    MessageBoxW, IDYES, MB_ICONERROR, MB_ICONINFORMATION, MB_ICONQUESTION, MB_OK, MB_YESNO,
};

const APP: &[u8] = include_bytes!(env!("DVSNIP_EXE"));
const CLI: &[u8] = include_bytes!(env!("DVSNIP_CLI_EXE"));
const VERSION: &str = env!("CARGO_PKG_VERSION");

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn dialog(quiet: bool, text: &str, error: bool) {
    if !quiet {
        let icon = if error { MB_ICONERROR } else { MB_ICONINFORMATION };
        unsafe { MessageBoxW(std::ptr::null_mut(), wide(text).as_ptr(), wide("dvsnip Setup").as_ptr(), MB_OK | icon) };
    }
}

fn ask(text: &str) -> bool {
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            wide(text).as_ptr(),
            wide("dvsnip Setup").as_ptr(),
            MB_YESNO | MB_ICONQUESTION,
        ) == IDYES
    }
}

fn install() -> Result<std::path::PathBuf, String> {
    let dir = install::install_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("Cannot create {}: {e}", dir.display()))?;
    let target = dir.join(install::EXE_NAME);
    install::replace_exe(&target, |t| std::fs::write(t, APP))?;
    install::replace_exe(&dir.join(install::CLI_EXE_NAME), |t| std::fs::write(t, CLI))?;
    install::finish_install(&target)?;
    Ok(target)
}

fn question(installed: Option<&str>) -> String {
    let what = "Print Screen will open dvsnip instead of the Windows screen capture: select an area and it is \
                copied to the clipboard, ready to paste.";
    match installed {
        None => format!(
            "Install dvsnip {VERSION} for your user account?\n\n{what}\n\nLocation: {}\nNo administrator rights needed.",
            install::install_dir().display()
        ),
        Some(v) if v == VERSION => format!("dvsnip {v} is already installed.\n\nReinstall it?"),
        Some(v) if install::is_newer(v, VERSION) => {
            format!("A newer dvsnip ({v}) is installed.\n\nReplace it with the older version {VERSION}?")
        }
        Some(v) => format!("dvsnip {v} is installed.\n\nUpdate it to {VERSION}? Your settings are kept."),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let quiet = args.iter().any(|a| a == "--quiet" || a.eq_ignore_ascii_case("/S"));

    if args.iter().any(|a| a == "--uninstall") {
        match install::uninstall() {
            Ok(()) => dialog(quiet, "dvsnip has been removed.", false),
            Err(e) => {
                dialog(quiet, &e, true);
                std::process::exit(1);
            }
        }
        return;
    }

    let installed = install::installed_version();
    if !quiet && !ask(&question(installed.as_deref())) {
        return;
    }
    match install() {
        Ok(exe) => {
            // the running copy keeps the old code; restart it from the new file
            install::quit_resident();
            let _ = std::process::Command::new(&exe).arg("--background").spawn();
            dialog(quiet, "dvsnip is installed.\n\nPress Print Screen, select an area, and paste.", false);
        }
        Err(e) => {
            dialog(quiet, &e, true);
            std::process::exit(1);
        }
    }
}
