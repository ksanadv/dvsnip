// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvsnip contributors
use crate::image::{self, Image};
use std::path::PathBuf;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HANDLE, HWND};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardSequenceNumber, OpenClipboard, RegisterClipboardFormatW,
    SetClipboardData,
};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};

const CF_DIB: u32 = 8;

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

pub fn loword(v: usize) -> u16 {
    (v & 0xFFFF) as u16
}

pub fn hiword(v: usize) -> u16 {
    ((v >> 16) & 0xFFFF) as u16
}

/// Signed client coordinates packed in a mouse message's LPARAM.
pub fn point(lp: isize) -> (i32, i32) {
    ((lp & 0xFFFF) as i16 as i32, ((lp >> 16) & 0xFFFF) as i16 as i32)
}

fn png_format() -> u32 {
    unsafe { RegisterClipboardFormatW(w!("PNG")) }
}

unsafe fn put(format: u32, bytes: &[u8]) -> bool {
    let Ok(h) = GlobalAlloc(GMEM_MOVEABLE, bytes.len()) else { return false };
    let p = GlobalLock(h) as *mut u8;
    if p.is_null() {
        let _ = windows::Win32::Foundation::GlobalFree(Some(h));
        return false;
    }
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
    let _ = GlobalUnlock(h);
    if SetClipboardData(format, Some(HANDLE(h.0))).is_err() {
        // the clipboard did not take ownership
        let _ = windows::Win32::Foundation::GlobalFree(Some(h));
        return false;
    }
    true
}

/// Replaces the clipboard with the image as a DIB. Returns the clipboard sequence number afterwards, so a PNG
/// encoded later can be added only if nothing else has been copied in between.
pub fn set_clipboard_image(owner: HWND, img: &Image) -> Option<u32> {
    let dib = image::dib(img);
    unsafe {
        OpenClipboard(Some(owner)).ok()?;
        let _ = EmptyClipboard();
        let ok = put(CF_DIB, &dib);
        let _ = CloseClipboard();
        ok.then(|| GetClipboardSequenceNumber())
    }
}

/// Adds the PNG form (kept alpha, preferred by browsers and chat apps) to an image this process just copied.
pub fn add_clipboard_png(owner: HWND, png: &[u8], seq: u32) {
    unsafe {
        if GetClipboardSequenceNumber() != seq || OpenClipboard(Some(owner)).is_err() {
            return;
        }
        put(png_format(), png);
        let _ = CloseClipboard();
    }
}

pub fn show_fatal(text: &str) {
    use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
    unsafe {
        MessageBoxW(None, PCWSTR(wide(text).as_ptr()), w!("dvsnip"), MB_OK | MB_ICONERROR);
    }
}

fn quiet() -> bool {
    std::env::args().any(|a| a == "--quiet")
}

pub fn message(text: &str, error: bool) {
    use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_ICONINFORMATION, MB_OK};
    if !quiet() {
        let icon = if error { MB_ICONERROR } else { MB_ICONINFORMATION };
        unsafe { MessageBoxW(None, PCWSTR(wide(text).as_ptr()), w!("dvsnip"), MB_OK | icon) };
    }
}

pub fn ask(text: &str) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, IDYES, MB_ICONQUESTION, MB_YESNO};
    unsafe { MessageBoxW(None, PCWSTR(wide(text).as_ptr()), w!("dvsnip"), MB_YESNO | MB_ICONQUESTION) == IDYES }
}

/// Hands the process's resident pages back to Windows; they come back on demand.
pub fn trim_working_set() {
    unsafe {
        let _ = windows::Win32::System::Memory::SetProcessWorkingSetSizeEx(
            windows::Win32::Foundation::HANDLE(-1isize as *mut std::ffi::c_void),
            usize::MAX,
            usize::MAX,
            windows::Win32::System::Memory::SETPROCESSWORKINGSETSIZEEX_FLAGS(0),
        );
    }
}

/// Undocumented but long-stable uxtheme entry points: 0 default, 1 allow dark, 2 force dark, 3 force light.
/// They make popup menus follow the theme.
pub fn set_app_mode(mode: i32) {
    unsafe {
        let Ok(m) = LoadLibraryW(w!("uxtheme.dll")) else { return };
        if let Some(f) = GetProcAddress(m, windows::core::PCSTR(135 as *const u8)) {
            let set_mode: extern "system" fn(i32) -> i32 = std::mem::transmute(f);
            set_mode(mode);
        }
        if let Some(f) = GetProcAddress(m, windows::core::PCSTR(136 as *const u8)) {
            let flush: extern "system" fn() = std::mem::transmute(f);
            flush();
        }
    }
}

/// The folder Windows itself saves screenshots to (Pictures\Screenshots, wherever Pictures lives).
pub fn screenshots_dir() -> Option<PathBuf> {
    use windows::Win32::UI::Shell::{FOLDERID_Screenshots, SHGetKnownFolderPath, KF_FLAG_CREATE};
    unsafe {
        let p = SHGetKnownFolderPath(&FOLDERID_Screenshots, KF_FLAG_CREATE, None).ok()?;
        let s = p.to_string().ok();
        windows::Win32::System::Com::CoTaskMemFree(Some(p.0 as *const _));
        s.map(PathBuf::from)
    }
}

/// `dir\name`, or `dir\name (2)` and so on when that already exists.
pub fn unique_path(dir: &std::path::Path, name: &str) -> PathBuf {
    let p = dir.join(name);
    if !p.exists() {
        return p;
    }
    let (stem, ext) = name.rsplit_once('.').unwrap_or((name, ""));
    (2..).map(|i| dir.join(format!("{stem} ({i}).{ext}"))).find(|p| !p.exists()).unwrap()
}

pub fn system_dark() -> bool {
    use winreg::enums::HKEY_CURRENT_USER;
    let light: u32 = winreg::RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize")
        .and_then(|k| k.get_value("AppsUseLightTheme"))
        .unwrap_or(1);
    light == 0
}

static LOG: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();

/// Timing lines for measuring, written only when DVSNIP_LOG names a file.
pub fn log(line: &str) {
    if let Some(p) = LOG.get_or_init(|| std::env::var_os("DVSNIP_LOG").map(PathBuf::from)) {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) {
            let _ = writeln!(f, "{line}");
        }
    }
}
