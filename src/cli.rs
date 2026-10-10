// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvsnip contributors
// dvsnip-cli: screen capture for scripts and AI agents. No tray and no windows (except the selection overlay for
// `snip`): a capture is one BitBlt (or PrintWindow for a window), a WIC encode and a file or stdout write.
// `dvsnip-cli mcp` serves the same captures to MCP clients over stdio.
use crate::capture::{self, WindowInfo};
use crate::image::{self, Image, Rect};
use crate::json::{obj, Value};
use crate::overlay::{self, Outcome};
use crate::png;
use crate::settings::Mode;
use crate::sys;
use std::io::Write;
use windows::Win32::Foundation::HWND;

/// The long-edge size Claude works with best; larger screenshots only cost more tokens.
pub const DEFAULT_MAX: u32 = 1568;

pub enum Target {
    Screen,
    Monitor(usize),
    Window(String),
    Hwnd(isize),
    Region(Rect),
    Foreground,
}

pub struct Capture {
    pub img: Image,
    /// what was captured, in virtual-desktop pixels
    pub rect: Rect,
    pub what: String,
}

fn window_label(w: &WindowInfo) -> String {
    format!("window \"{}\" ({})", w.title, if w.exe.is_empty() { &w.class } else { &w.exe })
}

/// The window whose title contains `query` (case-insensitive), preferring an exact title, then the frontmost.
pub fn find_window(query: &str) -> Result<WindowInfo, String> {
    let q = query.to_lowercase();
    let all = capture::list_windows();
    let mut hits: Vec<WindowInfo> = all.into_iter().filter(|w| w.title.to_lowercase().contains(&q)).collect();
    if hits.is_empty() {
        return Err(format!("no visible window has \"{query}\" in its title; list_windows shows what is open"));
    }
    let exact = hits.iter().position(|w| w.title.to_lowercase() == q).unwrap_or(0);
    Ok(hits.swap_remove(exact))
}

pub fn take(target: &Target) -> Result<Capture, String> {
    let screen = capture::virtual_screen();
    let region = |r: Rect, what: String| -> Result<Capture, String> {
        let r = r.intersect(&screen);
        let img = capture::grab_rect(r).ok_or("that area is outside the screen")?;
        Ok(Capture { img, rect: r, what })
    };
    match target {
        Target::Screen => region(screen, "the whole desktop".into()),
        Target::Monitor(i) => {
            let mons = capture::list_monitors();
            let m = mons.get(*i).ok_or_else(|| format!("there is no monitor {i}; there are {}", mons.len()))?;
            region(m.rect, format!("monitor {i}"))
        }
        Target::Region(r) => region(*r, "a region".into()),
        Target::Window(q) => {
            let w = find_window(q)?;
            let (img, rect) = capture::grab_window(HWND(w.hwnd as *mut _))?;
            Ok(Capture { img, rect, what: window_label(&w) })
        }
        Target::Hwnd(h) => {
            let w = capture::list_windows().into_iter().find(|w| w.hwnd == *h);
            let (img, rect) = capture::grab_window(HWND(*h as *mut _))?;
            Ok(Capture { img, rect, what: w.map_or_else(|| format!("window {h}"), |w| window_label(&w)) })
        }
        Target::Foreground => {
            let w =
                capture::list_windows().into_iter().find(|w| w.foreground).ok_or("no window is in the foreground")?;
            let (img, rect) = capture::grab_window(HWND(w.hwnd as *mut _))?;
            Ok(Capture { img, rect, what: window_label(&w) })
        }
    }
}

/// Lets the user select part of the screen with the overlay; runs a message loop until they finish.
pub fn user_snip(mode: Mode) -> Result<Capture, String> {
    use std::cell::RefCell;
    use std::rc::Rc;
    use windows::Win32::UI::WindowsAndMessaging::{DispatchMessageW, GetMessageW, TranslateMessage, MSG};
    let result: Rc<RefCell<Option<Outcome>>> = Rc::new(RefCell::new(None));
    let slot = result.clone();
    if !overlay::start(mode, sys::system_dark(), Box::new(move |out, _| *slot.borrow_mut() = Some(out))) {
        return Err("the screen could not be captured".into());
    }
    let mut msg = MSG::default();
    while result.borrow().is_none() {
        unsafe {
            if !GetMessageW(&mut msg, None, 0, 0).as_bool() {
                break;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    let outcome = result.borrow_mut().take();
    match outcome {
        Some(Outcome::Snip(img, rect)) => Ok(Capture { img, rect, what: "the user's selection".into() }),
        _ => Err("the user cancelled the snip".into()),
    }
}

/// The capture scaled for sending, and a description that maps image pixels back to the desktop.
pub fn describe(c: &Capture, sent: &Image, saved: Option<&str>) -> Value {
    let scale = sent.w as f64 / c.img.w.max(1) as f64;
    let mut fields = vec![
        ("captured", Value::from(c.what.clone())),
        ("x", c.rect.l.into()),
        ("y", c.rect.t.into()),
        ("width", c.rect.w().into()),
        ("height", c.rect.h().into()),
        ("image_width", sent.w.into()),
        ("image_height", sent.h.into()),
        ("scale", ((scale * 10000.0).round() / 10000.0).into()),
        ("note", "desktop_x = x + image_x / scale, desktop_y = y + image_y / scale (physical pixels)".into()),
    ];
    if let Some(p) = saved {
        fields.push(("saved", p.into()));
    }
    obj(&fields)
}

pub fn windows_json() -> Value {
    Value::Arr(
        capture::list_windows()
            .iter()
            .map(|w| {
                obj(&[
                    ("title", w.title.clone().into()),
                    ("process", w.exe.clone().into()),
                    ("pid", w.pid.into()),
                    ("hwnd", (w.hwnd as i64).into()),
                    ("x", w.rect.l.into()),
                    ("y", w.rect.t.into()),
                    ("width", w.rect.w().into()),
                    ("height", w.rect.h().into()),
                    ("minimized", w.minimized.into()),
                    ("foreground", w.foreground.into()),
                ])
            })
            .collect(),
    )
}

pub fn monitors_json() -> Value {
    Value::Arr(
        capture::list_monitors()
            .iter()
            .enumerate()
            .map(|(i, m)| {
                obj(&[
                    ("index", (i as i64).into()),
                    ("primary", m.primary.into()),
                    ("x", m.rect.l.into()),
                    ("y", m.rect.t.into()),
                    ("width", m.rect.w().into()),
                    ("height", m.rect.h().into()),
                    ("scale", (m.scale as f64).into()),
                ])
            })
            .collect(),
    )
}

pub fn parse_region(s: &str) -> Option<Rect> {
    let n: Vec<i32> =
        s.split([',', ' ', 'x']).filter(|p| !p.is_empty()).map(|p| p.trim().parse().ok()).collect::<Option<_>>()?;
    match n[..] {
        [x, y, w, h] if w > 0 && h > 0 => Some(Rect::new(x, y, x + w, y + h)),
        _ => None,
    }
}

pub fn parse_mode(s: &str) -> Mode {
    match s.to_ascii_lowercase().as_str() {
        "window" => Mode::Window,
        "freeform" | "free" | "lasso" => Mode::Freeform,
        _ => Mode::Rectangle,
    }
}

/// Puts an image on the clipboard from a process without windows of its own.
pub fn copy_to_clipboard(img: &Image) -> bool {
    use windows::core::w;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, HWND_MESSAGE, WINDOW_EX_STYLE, WINDOW_STYLE,
    };
    unsafe {
        // the clipboard needs an owner window; a message-only STATIC window will do
        let Ok(owner) = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("STATIC"),
            w!(""),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            None,
            None,
        ) else {
            return false;
        };
        let seq = sys::set_clipboard_image(owner, img);
        if let Some(seq) = seq {
            sys::add_clipboard_png(owner, &png::encode(img), seq);
        }
        let _ = DestroyWindow(owner);
        seq.is_some()
    }
}

const HELP: &str = "dvsnip-cli: screen capture for scripts and AI agents

usage:
  dvsnip-cli capture [target] [--max PX] [--out FILE | --out -] [--copy] [--delay S] [--json]
      target: (none) whole desktop | --monitor N | --window \"title part\" | --hwnd H
              | --region X,Y,W,H | --foreground
      --max PX   shrink so the longer side is at most PX (default: full size)
      --out FILE write there (PNG); '-' writes the PNG to stdout;
                 default: a new file in %TEMP%\\dvsnip
  dvsnip-cli snip [--mode rectangle|window|freeform] [--max PX] [--out FILE] [--copy] [--json]
      let the user select an area with the dvsnip overlay (Esc cancels, exit code 2)
  dvsnip-cli windows      visible windows, front to back, as JSON
  dvsnip-cli monitors     monitors as JSON
  dvsnip-cli mcp          serve the same as MCP tools on stdin/stdout

Coordinates are physical pixels of the virtual desktop. Exit code 0 on success, 1 on error.";

struct Args {
    rest: Vec<String>,
}

impl Args {
    fn flag(&mut self, name: &str) -> bool {
        let before = self.rest.len();
        self.rest.retain(|a| a != name);
        self.rest.len() != before
    }

    fn value(&mut self, name: &str) -> Result<Option<String>, String> {
        let Some(i) = self.rest.iter().position(|a| a == name) else { return Ok(None) };
        if i + 1 >= self.rest.len() {
            return Err(format!("{name} needs a value"));
        }
        let v = self.rest.remove(i + 1);
        self.rest.remove(i);
        Ok(Some(v))
    }
}

fn default_out(ext: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join("dvsnip");
    let _ = std::fs::create_dir_all(&dir);
    let t = crate::settings::local_time();
    let name = format!(
        "capture-{}{:02}{:02}-{:02}{:02}{:02}-{:03}.{ext}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond, t.wMilliseconds
    );
    sys::unique_path(&dir, &name)
}

fn output(c: Capture, a: &mut Args) -> Result<(), String> {
    let max: u32 =
        a.value("--max")?.map(|v| v.parse().map_err(|_| "--max takes a number of pixels")).transpose()?.unwrap_or(0);
    let out = a.value("--out")?;
    let copy = a.flag("--copy");
    let json = a.flag("--json");
    if let Some(extra) = a.rest.first() {
        return Err(format!("unknown argument {extra}"));
    }
    let img = image::fit_within(&c.img, max);
    if copy && !copy_to_clipboard(&img) {
        return Err("the clipboard is busy".into());
    }
    if out.as_deref() == Some("-") {
        std::io::stdout().write_all(&png::encode(&img)).map_err(|e| e.to_string())?;
        return Ok(());
    }
    let path = out.map(std::path::PathBuf::from).unwrap_or_else(|| default_out("png"));
    png::save(&img, &path).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    let shown = path.display().to_string();
    if json {
        println!("{}", describe(&c, &img, Some(&shown)).to_json());
    } else {
        println!("{shown}");
    }
    Ok(())
}

fn capture_cmd(a: &mut Args) -> Result<(), String> {
    let target = if let Some(n) = a.value("--monitor")? {
        Target::Monitor(n.parse().map_err(|_| "--monitor takes an index")?)
    } else if let Some(t) = a.value("--window")? {
        Target::Window(t)
    } else if let Some(h) = a.value("--hwnd")? {
        Target::Hwnd(h.parse().map_err(|_| "--hwnd takes a number")?)
    } else if let Some(r) = a.value("--region")? {
        Target::Region(parse_region(&r).ok_or("--region takes X,Y,W,H")?)
    } else if a.flag("--foreground") {
        Target::Foreground
    } else {
        Target::Screen
    };
    if let Some(d) = a.value("--delay")? {
        let s: f64 = d.parse().map_err(|_| "--delay takes seconds")?;
        std::thread::sleep(std::time::Duration::from_secs_f64(s.clamp(0.0, 60.0)));
    }
    output(take(&target)?, a)
}

/// Runs a command line; returns the process exit code.
pub fn run(args: Vec<String>) -> i32 {
    let mut a = Args { rest: args };
    let cmd = if a.rest.is_empty() { String::new() } else { a.rest.remove(0) };
    let result = match cmd.as_str() {
        "capture" | "shot" => capture_cmd(&mut a),
        "snip" => {
            let mode = a.value("--mode").ok().flatten().map(|m| parse_mode(&m)).unwrap_or(Mode::Rectangle);
            match user_snip(mode) {
                Ok(c) => output(c, &mut a),
                Err(e) => {
                    eprintln!("{e}");
                    return 2;
                }
            }
        }
        "windows" => {
            println!("{}", windows_json().to_json());
            Ok(())
        }
        "monitors" => {
            println!("{}", monitors_json().to_json());
            Ok(())
        }
        "mcp" => return crate::mcp::serve(),
        "" | "help" | "--help" | "-h" | "/?" => {
            println!("{HELP}");
            Ok(())
        }
        other => Err(format!("unknown command {other}; try dvsnip-cli --help")),
    };
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("dvsnip-cli: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regions_and_modes() {
        assert_eq!(parse_region("10,20,300,400"), Some(Rect::new(10, 20, 310, 420)));
        assert_eq!(parse_region("-1920, 0, 100, 50"), Some(Rect::new(-1920, 0, -1820, 50)));
        assert_eq!(parse_region("1,2,0,4"), None);
        assert_eq!(parse_region("1,2,3"), None);
        assert_eq!(parse_mode("Freeform"), Mode::Freeform);
        assert_eq!(parse_mode("anything"), Mode::Rectangle);
    }

    #[test]
    fn describe_maps_back_to_the_desktop() {
        let c = Capture {
            img: Image { w: 200, h: 100, px: vec![0; 20000] },
            rect: Rect::new(50, 60, 250, 160),
            what: "a region".into(),
        };
        let sent = image::fit_within(&c.img, 100);
        let d = describe(&c, &sent, None);
        assert_eq!(d.get("scale").and_then(Value::num), Some(0.5));
        assert_eq!(d.get("x").and_then(Value::int), Some(50));
        assert_eq!(d.get("image_width").and_then(Value::int), Some(100));
    }

    #[test]
    fn args_take_values_and_flags() {
        let mut a = Args { rest: vec!["--max".into(), "800".into(), "--json".into(), "--out".into(), "x.png".into()] };
        assert_eq!(a.value("--max").unwrap().as_deref(), Some("800"));
        assert!(a.flag("--json"));
        assert!(!a.flag("--copy"));
        assert_eq!(a.value("--out").unwrap().as_deref(), Some("x.png"));
        assert!(a.rest.is_empty());
        let mut b = Args { rest: vec!["--max".into()] };
        assert!(b.value("--max").is_err());
    }
}
