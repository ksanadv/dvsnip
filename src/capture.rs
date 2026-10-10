// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvsnip contributors
// Freezing the screen: one BitBlt of the whole virtual desktop into a DIB section, taken before any dvsnip window
// is shown, plus the list of top-level windows at that moment for window snips.
use crate::image::{Image, Rect};
use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, EnumDisplayMonitors, GetDC, GetMonitorInfoW,
    ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ, HMONITOR,
    MONITORINFO, SRCCOPY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetSystemMetrics, GetWindowThreadProcessId, IsIconic, IsWindowVisible,
    SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

/// A 32-bit top-down DIB section selected into its own memory DC, ready to BitBlt from.
pub struct Dib {
    pub dc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u32,
    pub w: i32,
    pub h: i32,
}

impl Dib {
    pub fn new(w: i32, h: i32) -> Option<Dib> {
        unsafe {
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w,
                    biHeight: -h,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits = std::ptr::null_mut();
            let bmp = CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
            let dc = CreateCompatibleDC(None);
            let old = SelectObject(dc, bmp.into());
            Some(Dib { dc, bmp, old, bits: bits as *mut u32, w, h })
        }
    }

    pub fn px(&self) -> &[u32] {
        unsafe { std::slice::from_raw_parts(self.bits, (self.w * self.h) as usize) }
    }

    pub fn px_mut(&mut self) -> &mut [u32] {
        unsafe { std::slice::from_raw_parts_mut(self.bits, (self.w * self.h) as usize) }
    }

    pub fn region(&self, r: Rect) -> Image {
        Image::from_region(self.px(), self.w as usize, self.h as usize, r)
    }
}

impl Drop for Dib {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            let _ = DeleteDC(self.dc);
            let _ = DeleteObject(self.bmp.into());
        }
    }
}

pub struct Shot {
    pub dib: Dib,
    /// virtual-desktop position of pixel (0, 0)
    pub origin: (i32, i32),
    /// monitors and visible top-level windows, front to back, relative to `origin`
    pub monitors: Vec<Rect>,
    pub windows: Vec<Rect>,
}

impl Shot {
    pub fn bounds(&self) -> Rect {
        Rect::new(0, 0, self.dib.w, self.dib.h)
    }

    /// The window under a point for a window snip, or the monitor when the point is over the desktop.
    pub fn window_at(&self, x: i32, y: i32) -> Option<Rect> {
        self.windows.iter().chain(&self.monitors).find(|r| r.contains(x, y)).copied()
    }

    pub fn monitor_at(&self, x: i32, y: i32) -> Rect {
        self.monitors.iter().find(|r| r.contains(x, y)).copied().unwrap_or(self.bounds())
    }
}

fn rect(r: RECT) -> Rect {
    Rect::new(r.left, r.top, r.right, r.bottom)
}

unsafe extern "system" fn monitor_proc(m: HMONITOR, _: HDC, _: *mut RECT, lp: LPARAM) -> BOOL {
    let out = &mut *(lp.0 as *mut Vec<Rect>);
    let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    if GetMonitorInfoW(m, &mut info).as_bool() {
        out.push(rect(info.rcMonitor));
    }
    true.into()
}

unsafe extern "system" fn window_proc(hwnd: HWND, lp: LPARAM) -> BOOL {
    let out = &mut *(lp.0 as *mut Vec<Rect>);
    if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
        return true.into();
    }
    let mut cloaked = 0u32;
    let _ = DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, &mut cloaked as *mut u32 as *mut _, 4);
    if cloaked != 0 {
        return true.into();
    }
    // the desktop itself counts as "the monitor under the cursor", not as one window spanning all of them
    let mut class = [0u16; 32];
    let n = GetClassNameW(hwnd, &mut class) as usize;
    let class = String::from_utf16_lossy(&class[..n]);
    if class == "Progman" || class == "WorkerW" {
        return true.into();
    }
    let mut pid = 0;
    GetWindowThreadProcessId(hwnd, Some(&mut pid));
    if pid == std::process::id() && (class == crate::overlay::CLASS_NAME || class == crate::overlay::DIM_CLASS_NAME) {
        return true.into();
    }
    let mut r = RECT::default();
    if DwmGetWindowAttribute(hwnd, DWMWA_EXTENDED_FRAME_BOUNDS, &mut r as *mut RECT as *mut _, 16).is_err() {
        return true.into();
    }
    let r = rect(r);
    if r.w() > 1 && r.h() > 1 {
        out.push(r);
    }
    true.into()
}

/// A screen copy running on its own thread, so the caller can create its windows while the pixels are copied.
pub struct PendingShot {
    dib: Dib,
    pub origin: (i32, i32),
    job: std::thread::JoinHandle<bool>,
}

/// Starts copying the whole virtual desktop into a DIB section.
pub fn begin_grab() -> Option<PendingShot> {
    let s = virtual_screen();
    if s.is_empty() {
        return None;
    }
    let dib = Dib::new(s.w(), s.h())?;
    let (dc, w, h, x, y) = (dib.dc.0 as isize, s.w(), s.h(), s.l, s.t);
    let job = std::thread::spawn(move || unsafe {
        let screen = GetDC(None);
        let ok = BitBlt(HDC(dc as *mut _), 0, 0, w, h, Some(screen), x, y, SRCCOPY).is_ok();
        ReleaseDC(None, screen);
        ok
    });
    Some(PendingShot { dib, origin: (s.l, s.t), job })
}

impl PendingShot {
    pub fn size(&self) -> (i32, i32) {
        (self.dib.w, self.dib.h)
    }

    /// Waits for the copy, then lists the monitors and windows as they were at that moment.
    pub fn finish(self) -> Option<Shot> {
        if !self.job.join().unwrap_or(false) {
            return None;
        }
        let (x, y) = self.origin;
        let (w, h) = (self.dib.w, self.dib.h);
        let mut monitors: Vec<Rect> = Vec::new();
        let mut windows: Vec<Rect> = Vec::new();
        unsafe {
            let _ = EnumDisplayMonitors(None, None, Some(monitor_proc), LPARAM(&mut monitors as *mut _ as isize));
            let _ = EnumWindows(Some(window_proc), LPARAM(&mut windows as *mut _ as isize));
        }
        let bounds = Rect::new(0, 0, w, h);
        let rel = |v: Vec<Rect>| -> Vec<Rect> {
            v.into_iter().map(|r| r.offset(-x, -y).intersect(&bounds)).filter(|r| !r.is_empty()).collect()
        };
        Some(Shot { dib: self.dib, origin: self.origin, monitors: rel(monitors), windows: rel(windows) })
    }
}

// ---- for the command line and the MCP server: targeted captures and what is on screen ----

pub fn virtual_screen() -> Rect {
    unsafe {
        let (x, y) = (GetSystemMetrics(SM_XVIRTUALSCREEN), GetSystemMetrics(SM_YVIRTUALSCREEN));
        Rect::new(x, y, x + GetSystemMetrics(SM_CXVIRTUALSCREEN), y + GetSystemMetrics(SM_CYVIRTUALSCREEN))
    }
}

/// Copies one rectangle of the virtual desktop (physical pixels), without capturing the rest.
pub fn grab_rect(r: Rect) -> Option<Image> {
    let r = r.intersect(&virtual_screen());
    if r.is_empty() {
        return None;
    }
    let dib = Dib::new(r.w(), r.h())?;
    unsafe {
        let screen = GetDC(None);
        let ok = BitBlt(dib.dc, 0, 0, r.w(), r.h(), Some(screen), r.l, r.t, SRCCOPY).is_ok();
        ReleaseDC(None, screen);
        ok.then(|| dib.region(Rect::new(0, 0, r.w(), r.h())))
    }
}

pub struct MonitorInfo {
    pub rect: Rect,
    pub work: Rect,
    pub primary: bool,
    pub scale: f32,
}

unsafe extern "system" fn monitor_info_proc(m: HMONITOR, _: HDC, _: *mut RECT, lp: LPARAM) -> BOOL {
    use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
    let out = &mut *(lp.0 as *mut Vec<MonitorInfo>);
    let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    if GetMonitorInfoW(m, &mut info).as_bool() {
        let (mut dx, mut dy) = (96u32, 96u32);
        let _ = GetDpiForMonitor(m, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
        out.push(MonitorInfo {
            rect: rect(info.rcMonitor),
            work: rect(info.rcWork),
            primary: info.dwFlags & 1 != 0,
            scale: dx as f32 / 96.0,
        });
    }
    true.into()
}

/// Monitors, primary first, in virtual-desktop coordinates.
pub fn list_monitors() -> Vec<MonitorInfo> {
    let mut v: Vec<MonitorInfo> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(monitor_info_proc), LPARAM(&mut v as *mut _ as isize));
    }
    v.sort_by_key(|m| (!m.primary, m.rect.t, m.rect.l));
    v
}

pub struct WindowInfo {
    pub hwnd: isize,
    pub title: String,
    pub class: String,
    pub pid: u32,
    pub exe: String,
    /// the visible frame, in virtual-desktop coordinates
    pub rect: Rect,
    pub minimized: bool,
    pub foreground: bool,
}

fn exe_name(pid: u32) -> String {
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return String::new() };
        let mut buf = [0u16; 512];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, windows::core::PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = windows::Win32::Foundation::CloseHandle(h);
        if ok.is_err() {
            return String::new();
        }
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        path.rsplit(['\\', '/']).next().unwrap_or("").to_owned()
    }
}

unsafe extern "system" fn list_proc(hwnd: HWND, lp: LPARAM) -> BOOL {
    use windows::Win32::UI::WindowsAndMessaging::{GetWindowTextLengthW, GetWindowTextW};
    let out = &mut *(lp.0 as *mut Vec<WindowInfo>);
    if !IsWindowVisible(hwnd).as_bool() {
        return true.into();
    }
    let mut cloaked = 0u32;
    let _ = DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, &mut cloaked as *mut u32 as *mut _, 4);
    let len = GetWindowTextLengthW(hwnd);
    if cloaked != 0 || len == 0 {
        return true.into();
    }
    let mut title = vec![0u16; len as usize + 1];
    let n = GetWindowTextW(hwnd, &mut title) as usize;
    let mut class = [0u16; 128];
    let cn = GetClassNameW(hwnd, &mut class) as usize;
    let mut pid = 0;
    GetWindowThreadProcessId(hwnd, Some(&mut pid));
    let mut r = RECT::default();
    let _ = DwmGetWindowAttribute(hwnd, DWMWA_EXTENDED_FRAME_BOUNDS, &mut r as *mut RECT as *mut _, 16);
    out.push(WindowInfo {
        hwnd: hwnd.0 as isize,
        title: String::from_utf16_lossy(&title[..n]),
        class: String::from_utf16_lossy(&class[..cn]),
        pid,
        exe: exe_name(pid),
        rect: rect(r),
        minimized: IsIconic(hwnd).as_bool(),
        foreground: windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow() == hwnd,
    });
    true.into()
}

/// Visible top-level windows that have a title, front to back.
pub fn list_windows() -> Vec<WindowInfo> {
    let mut v: Vec<WindowInfo> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(list_proc), LPARAM(&mut v as *mut _ as isize));
    }
    v
}

/// One window's own pixels, even where other windows cover it, and its frame in virtual-desktop coordinates.
/// Falls back to a copy of the screen area when the window cannot draw itself.
pub fn grab_window(hwnd: HWND) -> Result<(Image, Rect), String> {
    use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};
    use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;
    const PW_RENDERFULLCONTENT: u32 = 2;
    unsafe {
        if IsIconic(hwnd).as_bool() {
            return Err("the window is minimized".into());
        }
        let mut wr = RECT::default();
        GetWindowRect(hwnd, &mut wr).map_err(|e| e.message())?;
        let mut fr = wr;
        let _ = DwmGetWindowAttribute(hwnd, DWMWA_EXTENDED_FRAME_BOUNDS, &mut fr as *mut RECT as *mut _, 16);
        let (outer, frame) = (rect(wr), rect(fr));
        if let Some(dib) = Dib::new(outer.w(), outer.h()) {
            if PrintWindow(hwnd, dib.dc, PRINT_WINDOW_FLAGS(PW_RENDERFULLCONTENT)).as_bool() {
                let img = dib.region(frame.offset(-outer.l, -outer.t));
                // a window that cannot render off screen leaves the bitmap black
                if img.px.iter().any(|&p| p & 0x00FF_FFFF != 0) {
                    return Ok((img, frame));
                }
            }
        }
        grab_rect(frame).map(|img| (img, frame)).ok_or_else(|| "the window is not on screen".into())
    }
}
