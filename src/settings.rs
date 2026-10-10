// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvsnip contributors
use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Rectangle,
    Window,
    Full,
    Freeform,
}

#[derive(Clone)]
pub struct Settings {
    /// the selection mode the overlay opens in: the last one used
    pub mode: Mode,
    /// besides the clipboard, save every snip to the Screenshots folder
    pub auto_save: bool,
    /// Print Screen starts a snip
    pub take_prtsc: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { mode: Mode::Rectangle, auto_save: false, take_prtsc: true }
    }
}

fn path() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("APPDATA")?).join(crate::install::APP_NAME).join("settings.txt"))
}

impl Settings {
    pub fn load() -> Self {
        let mut s = Settings::default();
        let Some(text) = path().and_then(|p| std::fs::read_to_string(p).ok()) else { return s };
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            let v = v.trim();
            match k.trim() {
                "mode" => {
                    s.mode = match v {
                        "window" => Mode::Window,
                        "freeform" => Mode::Freeform,
                        _ => Mode::Rectangle,
                    }
                }
                "auto_save" => s.auto_save = v != "0",
                "print_screen" => s.take_prtsc = v != "0",
                _ => {}
            }
        }
        s
    }

    pub fn save(&self) {
        let Some(p) = path() else { return };
        let mode = match self.mode {
            Mode::Window => "window",
            Mode::Freeform => "freeform",
            Mode::Rectangle | Mode::Full => "rectangle",
        };
        let text = format!("mode={mode}\nauto_save={}\nprint_screen={}\n", self.auto_save as u8, self.take_prtsc as u8);
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(p, text);
    }
}

pub fn local_time() -> windows::Win32::Foundation::SYSTEMTIME {
    unsafe { windows::Win32::System::SystemInformation::GetLocalTime() }
}

/// The name Snipping Tool gives an automatic save: "Screenshot 2026-10-10 082701.png".
pub fn screenshot_name() -> String {
    let t = local_time();
    format!("Screenshot {}-{:02}-{:02} {:02}{:02}{:02}.png", t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond)
}
