// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvsnip contributors
// Print Screen while Windows keeps the key for its own screen capture. A registered hotkey is then never
// delivered, so a low-level keyboard hook swallows the key instead. The hook runs on its own thread with nothing
// else to do: Windows waits on it for every keystroke system-wide, so it must never sit behind UI work.
// Ctrl, Shift, Alt and Win + Print Screen are passed through untouched.
use std::sync::atomic::{AtomicIsize, Ordering};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VK_CONTROL, VK_LWIN, VK_RWIN, VK_SHIFT, VK_SNAPSHOT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetMessageW, PostMessageW, PostThreadMessageW, SetWindowsHookExW, UnhookWindowsHookEx,
    KBDLLHOOKSTRUCT, LLKHF_ALTDOWN, MSG, WH_KEYBOARD_LL, WM_KEYDOWN, WM_QUIT, WM_SYSKEYDOWN,
};

static TARGET: AtomicIsize = AtomicIsize::new(0);

pub struct Hook {
    thread: Option<std::thread::JoinHandle<()>>,
    tid: u32,
}

fn down(vk: u16) -> bool {
    unsafe { GetAsyncKeyState(vk as i32) as u16 & 0x8000 != 0 }
}

unsafe extern "system" fn proc(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code >= 0 {
        let k = &*(lp.0 as *const KBDLLHOOKSTRUCT);
        if k.vkCode == VK_SNAPSHOT.0 as u32
            && k.flags.0 & LLKHF_ALTDOWN.0 == 0
            && ![VK_CONTROL, VK_SHIFT, VK_LWIN, VK_RWIN].iter().any(|v| down(v.0))
        {
            if wp.0 == WM_KEYDOWN as usize || wp.0 == WM_SYSKEYDOWN as usize {
                let _ = PostMessageW(
                    Some(HWND(TARGET.load(Ordering::Relaxed) as *mut _)),
                    crate::resident::WM_TRIGGER,
                    WPARAM(0),
                    LPARAM(0),
                );
            }
            return LRESULT(1);
        }
    }
    CallNextHookEx(None, code, wp, lp)
}

pub fn start(target: HWND) -> Option<Hook> {
    TARGET.store(target.0 as isize, Ordering::Relaxed);
    let (tx, rx) = std::sync::mpsc::channel();
    let thread = std::thread::spawn(move || unsafe {
        let Ok(h) = SetWindowsHookExW(WH_KEYBOARD_LL, Some(proc), None, 0) else {
            let _ = tx.send(None);
            return;
        };
        let _ = tx.send(Some(GetCurrentThreadId()));
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {}
        let _ = UnhookWindowsHookEx(h);
    });
    let tid = rx.recv().ok().flatten()?;
    Some(Hook { thread: Some(thread), tid })
}

impl Drop for Hook {
    fn drop(&mut self) {
        unsafe {
            let _ = PostThreadMessageW(self.tid, WM_QUIT, WPARAM(0), LPARAM(0));
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
