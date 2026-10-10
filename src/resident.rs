// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvsnip contributors
// The background process: one hidden window that owns the Print Screen trigger and the clipboard. It has no tray
// icon and no menu; like the Snipping Tool it replaces it is only seen while a snip is being taken. It does nothing
// until a key press, or another dvsnip process (the Start menu shortcut, `--quit`, ...), wakes it.
use crate::image::Image;
use crate::overlay::{self, Outcome};
use crate::png;
use crate::settings::{Mode, Settings};
use crate::{hook, install, sys};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey, MOD_NOREPEAT, VK_SNAPSHOT};
use windows::Win32::UI::WindowsAndMessaging::*;

pub use crate::sys::log;

pub const WM_TRIGGER: u32 = WM_APP + 1;
const WM_ENCODED: u32 = WM_APP + 3;
const WM_RUN: u32 = WM_APP + 4;
const HOTKEY_PRTSC: i32 = 1;
// Explorer reacts to the Print Screen setting a moment after it changes, and a hotkey registered before that
// is silently dropped. Registering once more a little later keeps it.
const REARM_TIMER: usize = 1;
const REARM_MS: u32 = 1500;

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Command {
    /// start a snip, optionally in a given mode
    Snip(Option<Mode>),
    /// run in the background and wait for Print Screen
    Background,
    /// read the settings file again
    Reload,
    Quit,
}

impl Command {
    pub fn parse(arg: Option<&str>) -> Command {
        let Some(a) = arg else { return Command::Snip(None) };
        let lower = a.to_ascii_lowercase();
        // what Windows passes when dvsnip is chosen for these links (Win+Shift+S uses ms-screenclip)
        if lower.starts_with("ms-screenclip:") || lower.starts_with("ms-screensketch:") {
            let mode = if lower.contains("freeform") {
                Some(Mode::Freeform)
            } else if lower.contains("window") {
                Some(Mode::Window)
            } else if lower.contains("rectangle") {
                Some(Mode::Rectangle)
            } else {
                None
            };
            return Command::Snip(mode);
        }
        match lower.as_str() {
            "--background" => Command::Background,
            "--reload" => Command::Reload,
            "--quit" => Command::Quit,
            _ => Command::Snip(None),
        }
    }

    fn id(self) -> u32 {
        match self {
            Command::Snip(None) | Command::Snip(Some(Mode::Full)) => 0,
            Command::Snip(Some(Mode::Rectangle)) => 1,
            Command::Snip(Some(Mode::Window)) => 2,
            Command::Snip(Some(Mode::Freeform)) => 3,
            Command::Background => 4,
            Command::Reload => 5,
            Command::Quit => 6,
        }
    }

    fn from_id(id: u32) -> Command {
        match id {
            1 => Command::Snip(Some(Mode::Rectangle)),
            2 => Command::Snip(Some(Mode::Window)),
            3 => Command::Snip(Some(Mode::Freeform)),
            4 => Command::Background,
            5 => Command::Reload,
            6 => Command::Quit,
            _ => Command::Snip(None),
        }
    }
}

struct Resident {
    hwnd: HWND,
    settings: Settings,
    hotkey: bool,
    hook: Option<hook::Hook>,
    taskbar_created: u32,
    encoding: usize,
}

thread_local! {
    static RES: RefCell<Option<Resident>> = const { RefCell::new(None) };
    static HWND_CELL: Cell<isize> = const { Cell::new(0) };
}

fn with<R>(f: impl FnOnce(&mut Resident) -> R) -> Option<R> {
    RES.with(|c| c.try_borrow_mut().ok().and_then(|mut g| g.as_mut().map(f)))
}

fn owner() -> HWND {
    HWND(HWND_CELL.with(Cell::get) as *mut _)
}

fn settings() -> Settings {
    with(|r| r.settings.clone()).unwrap_or_else(Settings::load)
}

// ---- snipping ----

fn new_snip(mode: Option<Mode>) {
    let s = settings();
    overlay::start(
        mode.unwrap_or(s.mode),
        sys::system_dark(),
        Box::new(|out, mode| {
            if mode != Mode::Full && mode != settings().mode {
                with(|r| {
                    r.settings.mode = mode;
                    r.settings.save();
                });
            }
            if let Outcome::Snip(img, _) = out {
                publish(img);
            }
            trim_if_idle();
        }),
    );
}

struct Encoded {
    png: Vec<u8>,
    seq: Option<u32>,
    saved: Option<Result<PathBuf, String>>,
}

/// Puts a snip on the clipboard right away, then encodes it as PNG on a worker thread: the PNG is added to the
/// clipboard (if nothing was copied meanwhile) and, when asked, saved to the Screenshots folder.
fn publish(img: Image) {
    let hwnd = owner();
    let seq = sys::set_clipboard_image(hwnd, &img);
    let dir = if settings().auto_save { sys::screenshots_dir() } else { None };
    let target = hwnd.0 as isize;
    with(|r| r.encoding += 1);
    std::thread::spawn(move || {
        let t = std::time::Instant::now();
        let png = png::encode(&img);
        log(&format!("encode {}x{}: {:.1} ms, {} bytes", img.w, img.h, t.elapsed().as_secs_f64() * 1000.0, png.len()));
        drop(img);
        let saved = dir.map(|dir| {
            let path = sys::unique_path(&dir, &crate::settings::screenshot_name());
            png::write_file(&path, &png).map(|()| path)
        });
        let msg = Box::into_raw(Box::new(Encoded { png, seq, saved }));
        unsafe {
            if PostMessageW(Some(HWND(target as *mut _)), WM_ENCODED, WPARAM(0), LPARAM(msg as isize)).is_err() {
                drop(Box::from_raw(msg));
            }
        }
    });
}

fn encoded(e: Box<Encoded>) {
    if let Some(seq) = e.seq {
        sys::add_clipboard_png(owner(), &e.png, seq);
    }
    if let Some(Err(err)) = &e.saved {
        sys::message(&format!("The snip is on the clipboard but could not be saved.\n\n{err}"), true);
    }
    drop(e);
    with(|r| r.encoding = r.encoding.saturating_sub(1));
    trim_if_idle();
}

/// Returns idle memory to Windows once no snip or encoder is alive.
fn trim_if_idle() {
    if with(|r| r.encoding == 0) == Some(true) && !overlay::active() {
        sys::trim_working_set();
    }
}

// ---- Print Screen ----

/// Chooses how Print Screen reaches dvsnip: a registered hotkey when Windows has let go of the key, the keyboard
/// hook while Windows still claims it, or nothing when the user turned the trigger off.
fn arm(r: &mut Resident) {
    let want = r.settings.take_prtsc;
    let windows_owns = install::windows_takes_print_screen();
    unsafe {
        if want && !windows_owns {
            if !r.hotkey {
                r.hotkey = RegisterHotKey(Some(r.hwnd), HOTKEY_PRTSC, MOD_NOREPEAT, VK_SNAPSHOT.0 as u32).is_ok();
                if r.hotkey {
                    SetTimer(Some(r.hwnd), REARM_TIMER, REARM_MS, None);
                }
            }
        } else if r.hotkey {
            let _ = UnregisterHotKey(Some(r.hwnd), HOTKEY_PRTSC);
            r.hotkey = false;
        }
    }
    let need_hook = want && (windows_owns || !r.hotkey);
    if need_hook && r.hook.is_none() {
        r.hook = hook::start(r.hwnd);
    } else if !need_hook {
        r.hook = None;
    }
    log(&format!("print screen: hotkey={} hook={}", r.hotkey, r.hook.is_some()));
}

/// Registers the Print Screen hotkey again, in case Windows dropped it.
fn rearm(r: &mut Resident) {
    if r.hotkey {
        unsafe {
            let _ = UnregisterHotKey(Some(r.hwnd), HOTKEY_PRTSC);
            r.hotkey = RegisterHotKey(Some(r.hwnd), HOTKEY_PRTSC, MOD_NOREPEAT, VK_SNAPSHOT.0 as u32).is_ok();
        }
        if !r.hotkey {
            arm(r);
        }
    }
}

fn run_command(cmd: Command) {
    match cmd {
        Command::Snip(mode) => new_snip(mode),
        Command::Background => {}
        Command::Reload => {
            with(|r| {
                r.settings = Settings::load();
                arm(r);
            });
        }
        Command::Quit => unsafe {
            let _ = DestroyWindow(owner());
        },
    }
}

extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_HOTKEY if wp.0 == HOTKEY_PRTSC as usize => {
                log("trigger: hotkey");
                new_snip(None);
                LRESULT(0)
            }
            WM_TRIGGER => {
                log("trigger: hook");
                new_snip(None);
                LRESULT(0)
            }
            WM_TIMER if wp.0 == REARM_TIMER => {
                let _ = KillTimer(Some(hwnd), REARM_TIMER);
                with(rearm);
                LRESULT(0)
            }
            WM_ENCODED => {
                encoded(Box::from_raw(lp.0 as *mut Encoded));
                LRESULT(0)
            }
            WM_COPYDATA => {
                let d = &*(lp.0 as *const COPYDATASTRUCT);
                if d.dwData == 1 {
                    let bytes: &[u8] = if d.lpData.is_null() {
                        &[]
                    } else {
                        std::slice::from_raw_parts(d.lpData as *const u8, d.cbData as usize)
                    };
                    let text = String::from_utf8_lossy(bytes);
                    let cmd = Command::parse(Some(text.as_ref()).filter(|s| !s.is_empty()));
                    // answer the sender first; a snip shows UI that must not keep it waiting
                    let _ = PostMessageW(Some(hwnd), WM_RUN, WPARAM(cmd.id() as usize), LPARAM(0));
                }
                LRESULT(1)
            }
            WM_RUN => {
                run_command(Command::from_id(wp.0 as u32));
                LRESULT(0)
            }
            WM_SETTINGCHANGE => {
                // the Print Screen setting may have changed
                with(arm);
                LRESULT(0)
            }
            WM_DESTROY => {
                overlay::cancel();
                with(|r| {
                    r.hook = None;
                    if r.hotkey {
                        let _ = UnregisterHotKey(Some(hwnd), HOTKEY_PRTSC);
                    }
                });
                PostQuitMessage(0);
                LRESULT(0)
            }
            m if m != 0 && with(|r| r.taskbar_created == m).unwrap_or(false) => {
                // Explorer restarted: the hotkey may have been dropped
                with(rearm);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

/// Runs the resident process, or hands the command-line argument to the one already running.
pub fn run(arg: Option<&str>) {
    let cmd = Command::parse(arg);
    unsafe {
        let mutex = windows::Win32::System::Threading::CreateMutexW(None, false, w!("Local\\dvsnip-resident"));
        let exists = windows::Win32::Foundation::GetLastError() == windows::Win32::Foundation::ERROR_ALREADY_EXISTS;
        if exists {
            // the first instance may still be creating its window
            for _ in 0..40 {
                if install::send_to_resident(arg.unwrap_or("")) {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            return;
        }
        if matches!(cmd, Command::Quit | Command::Reload) {
            return;
        }

        let hinst = windows::Win32::System::LibraryLoader::GetModuleHandleW(None).unwrap_or_default();
        let class = PCWSTR(sys::wide(install::RESIDENT_CLASS).leak().as_ptr());
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(proc),
            hInstance: hinst.into(),
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassExW(&wc);
        // a hidden top-level window rather than a message-only one: only top-level windows hear TaskbarCreated
        let Ok(hwnd) = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            class,
            w!("dvsnip"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinst.into()),
            None,
        ) else {
            return;
        };
        HWND_CELL.with(|c| c.set(hwnd.0 as isize));
        let taskbar_created = RegisterWindowMessageW(w!("TaskbarCreated"));
        RES.with(|c| {
            *c.borrow_mut() = Some(Resident {
                hwnd,
                settings: Settings::load(),
                hotkey: false,
                hook: None,
                taskbar_created,
                encoding: 0,
            })
        });
        with(arm);
        if cmd != Command::Background {
            let _ = PostMessageW(Some(hwnd), WM_RUN, WPARAM(cmd.id() as usize), LPARAM(0));
        }
        sys::trim_working_set();

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        RES.with(|c| c.borrow_mut().take());
        let _ = windows::Win32::Foundation::CloseHandle(mutex.unwrap_or_default());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_commands() {
        assert_eq!(Command::parse(None), Command::Snip(None));
        assert_eq!(Command::parse(Some("--background")), Command::Background);
        assert_eq!(Command::parse(Some("--reload")), Command::Reload);
        assert_eq!(Command::parse(Some("ms-screenclip:")), Command::Snip(None));
        assert_eq!(
            Command::parse(Some("ms-screenclip://capture/image?freeform&user-agent=x")),
            Command::Snip(Some(Mode::Freeform))
        );
        assert_eq!(Command::parse(Some("MS-SCREENSKETCH:edit")), Command::Snip(None));
        for c in [Command::Snip(None), Command::Snip(Some(Mode::Window)), Command::Reload, Command::Quit] {
            assert_eq!(Command::from_id(c.id()), c);
        }
    }
}
