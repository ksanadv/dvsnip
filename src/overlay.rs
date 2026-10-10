// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvsnip contributors
// The capture overlay: the frozen screen, dimmed, with the selection shown at full brightness and the mode toolbar
// at the top. Plain GDI. Only the capture is kept in memory; each paint dims the changed area through a small
// strip buffer and BitBlts it, so dragging a selection over a 4K screen costs a few small memory copies per frame.
//
// To appear at once, a plain half-transparent black window covers the screen first. It is excluded from screen
// capture, so the screen copy taken right after does not contain it, and it looks exactly like the dimmed copy
// that replaces it a few dozen milliseconds later.
use crate::capture::{self, Dib, Shot};
use crate::glyphs::{self, Icon};
use crate::image::{Image, Rect};
use crate::settings::Mode;
use crate::sys::{self, point};
use std::cell::RefCell;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_CLOAK, DWMWA_TRANSITIONS_FORCEDISABLED};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, ReleaseCapture, SetCapture, UnregisterHotKey, HOT_KEY_MODIFIERS, VK_ESCAPE,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::Win32::UI::WindowsAndMessaging::{
    SetLayeredWindowAttributes, SetWindowDisplayAffinity, WDA_EXCLUDEFROMCAPTURE,
};

pub const CLASS_NAME: &str = "dvsnipOverlay";
pub const DIM_CLASS_NAME: &str = "dvsnipDim";
/// rows dimmed per strip while painting
const BAND: i32 = 64;
const ESC_HOTKEY: i32 = 2;

#[derive(Clone, Copy, PartialEq)]
enum Button {
    Mode(Mode),
    FullScreen,
    Close,
}

const BUTTONS: [(Button, Icon); 5] = [
    (Button::Mode(Mode::Rectangle), Icon::RectSnip),
    (Button::Mode(Mode::Window), Icon::WindowSnip),
    (Button::FullScreen, Icon::FullSnip),
    (Button::Mode(Mode::Freeform), Icon::FreeSnip),
    (Button::Close, Icon::Close),
];

struct Colors {
    bar: u32,
    border: u32,
    hot: u32,
    selected: u32,
    fg: u32,
    accent: u32,
}

fn colors(dark: bool) -> Colors {
    if dark {
        Colors { bar: 0x2B2B2B, border: 0x454545, hot: 0x3A3A3A, selected: 0x474747, fg: 0xFFFFFF, accent: 0x60CDFF }
    } else {
        Colors { bar: 0xF9F9F9, border: 0xD5D5D5, hot: 0xECECEC, selected: 0xE0E0E0, fg: 0x1B1B1B, accent: 0x005FB8 }
    }
}

/// 0xRRGGBB to GDI's 0x00BBGGRR
fn cref(c: u32) -> COLORREF {
    COLORREF(((c & 0xFF) << 16) | (c & 0xFF00) | ((c >> 16) & 0xFF))
}

/// What the overlay produced; handed to the resident process once the overlay is gone.
pub enum Outcome {
    /// the pixels and where they were, in virtual-desktop coordinates
    Snip(Image, Rect),
    Cancel,
}

struct Overlay {
    hwnd: HWND,
    shot: Shot,
    /// a few rows of dimmed screen, reused by every paint
    strip: Option<Dib>,
    mode: Mode,
    anchor: Option<(i32, i32)>,
    sel: Rect,
    lasso: Vec<(i32, i32)>,
    hover: Option<Rect>,
    label: Rect,
    bar: Rect,
    bar_dib: Option<Dib>,
    hot: Option<usize>,
    pressed: Option<usize>,
    scale: f32,
    text_font: HFONT,
    colors: Colors,
    was_active: bool,
}

thread_local! {
    static OVERLAY: RefCell<Option<Overlay>> = const { RefCell::new(None) };
    static DONE: RefCell<Option<Box<dyn FnOnce(Outcome, Mode)>>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut Overlay) -> R) -> Option<R> {
    OVERLAY.with(|c| c.try_borrow_mut().ok().and_then(|mut g| g.as_mut().map(f)))
}

pub fn active() -> bool {
    OVERLAY.with(|c| c.try_borrow().map_or(true, |g| g.is_some()))
}

fn font(face: PCWSTR, px: f32, weight: i32) -> HFONT {
    unsafe {
        CreateFontW(
            -(px.round() as i32),
            0,
            0,
            0,
            weight,
            0,
            0,
            0,
            Default::default(),
            Default::default(),
            Default::default(),
            CLEARTYPE_QUALITY,
            Default::default(),
            face,
        )
    }
}

/// Fills a rounded rectangle into a DIB with antialiased edges (signed distance coverage); `r` is in DIB pixels.
fn fill_round(dib: &mut Dib, r: Rect, radius: f32, color: u32) {
    let (w, h) = (dib.w, dib.h);
    let (cx, cy) = ((r.l + r.r) as f32 / 2.0, (r.t + r.b) as f32 / 2.0);
    let (hx, hy) = (r.w() as f32 / 2.0, r.h() as f32 / 2.0);
    let rad = radius.min(hx).min(hy);
    let px = dib.px_mut();
    for y in r.t.max(0)..r.b.min(h) {
        for x in r.l.max(0)..r.r.min(w) {
            let qx = ((x as f32 + 0.5 - cx).abs() - (hx - rad)).max(0.0);
            let qy = ((y as f32 + 0.5 - cy).abs() - (hy - rad)).max(0.0);
            let d = (qx * qx + qy * qy).sqrt() - rad;
            let cov = (0.5 - d).clamp(0.0, 1.0);
            if cov <= 0.0 {
                continue;
            }
            let p = &mut px[(y * w + x) as usize];
            let mix = |s: u32| {
                let a = ((*p >> s) & 0xFF) as f32;
                let b = ((color >> s) & 0xFF) as f32;
                ((a + (b - a) * cov).round() as u32) << s
            };
            *p = 0xFF00_0000 | mix(16) | mix(8) | mix(0);
        }
    }
}

/// Paints `color` into a DIB through a square coverage mask placed at (x, y).
fn blend_mask(dib: &mut Dib, mask: &[u8], size: i32, x: i32, y: i32, color: u32) {
    let (w, h) = (dib.w, dib.h);
    let px = dib.px_mut();
    for my in 0..size {
        for mx in 0..size {
            let (dx, dy) = (x + mx, y + my);
            let a = mask[(my * size + mx) as usize] as u32;
            if a == 0 || dx < 0 || dy < 0 || dx >= w || dy >= h {
                continue;
            }
            let p = &mut px[(dy * w + dx) as usize];
            let mix = |s: u32| (((*p >> s) & 0xFF) * (255 - a) + ((color >> s) & 0xFF) * a + 127) / 255;
            *p = 0xFF00_0000 | mix(16) << 16 | mix(8) << 8 | mix(0);
        }
    }
}

unsafe fn text(hdc: HDC, s: &str, mut r: RECT, color: u32, f: HFONT) {
    let mut w16: Vec<u16> = s.encode_utf16().collect();
    SelectObject(hdc, f.into());
    SetBkMode(hdc, TRANSPARENT);
    SetTextColor(hdc, cref(color));
    DrawTextW(hdc, &mut w16, &mut r, DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX);
}

unsafe fn text_size(hdc: HDC, s: &str, f: HFONT) -> (i32, i32) {
    let w16: Vec<u16> = s.encode_utf16().collect();
    let old = SelectObject(hdc, f.into());
    let mut size = Default::default();
    let _ = GetTextExtentPoint32W(hdc, &w16, &mut size);
    SelectObject(hdc, old);
    (size.cx, size.cy)
}

/// Half brightness, the same as the black layer at alpha 128 that is shown before the capture is ready.
fn dim(p: u32) -> u32 {
    ((p >> 1) & 0x007F_7F7F) | 0xFF00_0000
}

fn rc(r: Rect) -> RECT {
    RECT { left: r.l, top: r.t, right: r.r, bottom: r.b }
}

impl Overlay {
    fn px(&self, v: f32) -> i32 {
        (v * self.scale).round() as i32
    }

    fn button_rect(&self, i: usize) -> Rect {
        let (pad, bw, gap, sep) = (self.px(6.0), self.px(40.0), self.px(2.0), self.px(13.0));
        let mut x = self.bar.l + pad + i as i32 * (bw + gap);
        if i == BUTTONS.len() - 1 {
            x += sep - gap;
        }
        Rect::new(x, self.bar.t + pad, x + bw, self.bar.b - pad)
    }

    fn layout_bar(&mut self) {
        let (pad, bw, gap, sep, bh) = (self.px(6.0), self.px(40.0), self.px(2.0), self.px(13.0), self.px(36.0));
        let n = BUTTONS.len() as i32;
        let width = pad * 2 + n * bw + (n - 2) * gap + sep;
        let mut cur = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut cur);
        }
        let m = self.shot.monitor_at(cur.x - self.shot.origin.0, cur.y - self.shot.origin.1);
        let l = (m.l + m.r) / 2 - width / 2;
        let t = m.t + self.px(16.0);
        self.bar = Rect::new(l, t, l + width, t + bh + pad * 2);
    }

    fn button_at(&self, x: i32, y: i32) -> Option<usize> {
        if !self.bar.contains(x, y) || self.dragging() {
            return None;
        }
        (0..BUTTONS.len()).find(|&i| self.button_rect(i).contains(x, y))
    }

    fn dragging(&self) -> bool {
        self.anchor.is_some()
    }

    fn bar_visible(&self) -> bool {
        !self.dragging()
    }

    /// The area shown at full brightness.
    fn bright(&self) -> Rect {
        match self.mode {
            Mode::Rectangle => self.sel,
            Mode::Window | Mode::Full => self.hover.unwrap_or_default(),
            Mode::Freeform => Rect::default(),
        }
    }

    /// Everything the selection draws: the bright area, its frame and the size label.
    fn decor(&self) -> Rect {
        let b = self.bright();
        let frame = if b.is_empty() { b } else { b.inflate(self.px(2.0)) };
        frame.union(&self.label)
    }

    fn invalidate(&self, r: Rect) {
        if !r.is_empty() {
            unsafe {
                let _ = InvalidateRect(Some(self.hwnd), Some(&rc(r)), false);
            }
        }
    }

    fn place_label(&mut self, hdc: HDC) {
        let b = self.bright();
        if b.is_empty() || self.mode == Mode::Freeform {
            self.label = Rect::default();
            return;
        }
        let s = format!("{} × {}", b.w(), b.h());
        let (tw, th) = unsafe { text_size(hdc, &s, self.text_font) };
        let (pw, ph) = (tw + self.px(16.0), th + self.px(8.0));
        let gap = self.px(8.0);
        let below = b.b + gap + ph <= self.shot.dib.h;
        let t = if below { b.b + gap } else { (b.t - gap - ph).max(0) };
        let l = b.l.clamp(0, (self.shot.dib.w - pw).max(0));
        self.label = Rect::new(l, t, l + pw, t + ph);
    }

    fn render_bar(&mut self) {
        let (w, h) = (self.bar.w(), self.bar.h());
        if self.bar_dib.as_ref().is_none_or(|d| d.w != w || d.h != h) {
            self.bar_dib = Dib::new(w, h);
        }
        let Some(mut dib) = self.bar_dib.take() else { return };
        // start from the dimmed screen under the bar so the rounded corners blend into it
        let (sw, sh) = (self.shot.dib.w, self.shot.dib.h);
        let src = self.shot.dib.px();
        for y in 0..h {
            let sy = (self.bar.t + y).clamp(0, sh - 1);
            for x in 0..w {
                let sx = (self.bar.l + x).clamp(0, sw - 1);
                dib.px_mut()[(y * w + x) as usize] = dim(src[(sy * sw + sx) as usize]);
            }
        }
        let radius = self.px(8.0) as f32;
        let local = Rect::new(0, 0, w, h);
        fill_round(&mut dib, local, radius, self.colors.border);
        fill_round(&mut dib, local.inflate(-self.px(1.0).max(1)), radius - 1.0, self.colors.bar);
        for (i, (b, _)) in BUTTONS.iter().enumerate() {
            let r = self.button_rect(i).offset(-self.bar.l, -self.bar.t);
            let selected = *b == Button::Mode(self.mode);
            let fill = if self.pressed == Some(i) || selected {
                Some(self.colors.selected)
            } else if self.hot == Some(i) {
                Some(self.colors.hot)
            } else {
                None
            };
            if let Some(c) = fill {
                fill_round(&mut dib, r, self.px(4.0) as f32, c);
            }
        }
        // separator before Close
        let last = self.button_rect(BUTTONS.len() - 1).offset(-self.bar.l, -self.bar.t);
        let sx = last.l - self.px(7.0);
        fill_round(
            &mut dib,
            Rect::new(sx, last.t + self.px(8.0), sx + self.px(1.0).max(1), last.b - self.px(8.0)),
            0.0,
            self.colors.border,
        );
        let size = self.px(16.0) as u32;
        for (i, (b, icon)) in BUTTONS.iter().enumerate() {
            let r = self.button_rect(i).offset(-self.bar.l, -self.bar.t);
            let color = if *b == Button::Mode(self.mode) { self.colors.accent } else { self.colors.fg };
            let (x, y) = (r.l + (r.w() - size as i32) / 2, r.t + (r.h() - size as i32) / 2);
            blend_mask(&mut dib, &glyphs::mask(*icon, size), size as i32, x, y, color);
        }
        self.bar_dib = Some(dib);
    }

    /// Copies `area` of the frozen screen to the window, half brightness outside `bright`.
    unsafe fn paint_screen(&mut self, hdc: HDC, area: Rect, bright: Rect) {
        let area = area.intersect(&self.shot.bounds());
        if area.is_empty() {
            return;
        }
        let w = area.w();
        if self.strip.as_ref().is_none_or(|s| s.w < w) {
            self.strip = Dib::new(w, BAND);
        }
        let Some(strip) = self.strip.as_mut() else { return };
        let (sw, stride, dc) = (self.shot.dib.w as usize, strip.w as usize, strip.dc);
        let src = self.shot.dib.px();
        let dst = strip.px_mut();
        let mut y0 = area.t;
        while y0 < area.b {
            let y1 = (y0 + BAND).min(area.b);
            for y in y0..y1 {
                let row = &src[y as usize * sw + area.l as usize..y as usize * sw + area.r as usize];
                let out = &mut dst[(y - y0) as usize * stride..(y - y0) as usize * stride + w as usize];
                // the bright span of this row, relative to the area
                let (a, b) = if y >= bright.t && y < bright.b {
                    ((bright.l - area.l).clamp(0, w) as usize, (bright.r - area.l).clamp(0, w) as usize)
                } else {
                    (0, 0)
                };
                for (o, &p) in out[..a].iter_mut().zip(&row[..a]) {
                    *o = dim(p);
                }
                out[a..b].copy_from_slice(&row[a..b]);
                for (o, &p) in out[b..].iter_mut().zip(&row[b..]) {
                    *o = dim(p);
                }
            }
            let _ = BitBlt(hdc, area.l, y0, w, y1 - y0, Some(dc), 0, 0, SRCCOPY);
            y0 = y1;
        }
    }

    fn paint(&mut self) {
        unsafe {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(self.hwnd, &mut ps);
            let area = Rect::new(ps.rcPaint.left, ps.rcPaint.top, ps.rcPaint.right, ps.rcPaint.bottom);
            let bright = self.bright();
            let bar = if self.bar_visible() { self.bar } else { Rect::default() };

            // the screen, dimmed outside the bright area, a strip of rows at a time; the toolbar is left out so no
            // pixel is drawn twice
            let saved = SaveDC(hdc);
            if !bar.is_empty() {
                ExcludeClipRect(hdc, bar.l, bar.t, bar.r, bar.b);
            }
            self.paint_screen(hdc, area, bright);
            let _ = RestoreDC(hdc, saved);

            let lit = bright.intersect(&area);
            if !lit.is_empty() {
                // a thin frame just outside the selection
                let t = self.px(1.0).max(1);
                let brush = CreateSolidBrush(cref(0xFFFFFF));
                let f = bright.inflate(t);
                for r in [
                    Rect::new(f.l, f.t, f.r, bright.t),
                    Rect::new(f.l, bright.b, f.r, f.b),
                    Rect::new(f.l, bright.t, bright.l, bright.b),
                    Rect::new(bright.r, bright.t, f.r, bright.b),
                ] {
                    FillRect(hdc, &rc(r), brush);
                }
                let _ = DeleteObject(brush.into());
            }

            if self.mode == Mode::Freeform && self.lasso.len() > 1 {
                let pen = CreatePen(PS_SOLID, self.px(2.0).max(1), cref(0xFFFFFF));
                let old = SelectObject(hdc, pen.into());
                let pts: Vec<POINT> = self.lasso.iter().map(|&(x, y)| POINT { x, y }).collect();
                let _ = Polyline(hdc, &pts);
                SelectObject(hdc, old);
                let _ = DeleteObject(pen.into());
            }

            self.place_label(hdc);
            if !self.label.is_empty() {
                let brush = CreateSolidBrush(cref(self.colors.bar));
                FillRect(hdc, &rc(self.label), brush);
                let _ = DeleteObject(brush.into());
                let b = self.bright();
                text(hdc, &format!("{} × {}", b.w(), b.h()), rc(self.label), self.colors.fg, self.text_font);
            }

            if !bar.is_empty() && !bar.intersect(&area).is_empty() {
                self.render_bar();
                if let Some(d) = &self.bar_dib {
                    let _ = BitBlt(hdc, bar.l, bar.t, bar.w(), bar.h(), Some(d.dc), 0, 0, SRCCOPY);
                }
            }
            let _ = EndPaint(self.hwnd, &ps);
        }
    }

    fn set_hot(&mut self, hot: Option<usize>) {
        if hot != self.hot {
            self.hot = hot;
            self.invalidate(self.bar);
        }
    }

    fn set_mode(&mut self, mode: Mode) {
        let before = self.decor();
        self.mode = mode;
        self.sel = Rect::default();
        self.lasso.clear();
        self.hover = None;
        self.invalidate(before.union(&self.bar));
    }

    fn update_hover(&mut self, x: i32, y: i32) {
        let hover = match self.mode {
            Mode::Window => self.shot.window_at(x, y),
            Mode::Full => Some(self.shot.monitor_at(x, y)),
            _ => None,
        };
        if hover != self.hover {
            let before = self.decor();
            self.hover = hover;
            self.label = Rect::default();
            self.invalidate(before.union(&self.decor().union(&self.label_room())));
        }
    }

    /// Where a size label for the current selection could go, so the area is repainted before it is placed.
    fn label_room(&self) -> Rect {
        let b = self.bright();
        if b.is_empty() {
            return b;
        }
        let room = Rect::new(b.l, b.t - self.px(48.0), b.l + self.px(160.0), b.b + self.px(48.0));
        room.intersect(&self.shot.bounds())
    }

    fn mouse_move(&mut self, x: i32, y: i32) {
        let hot = self.button_at(x, y);
        self.set_hot(hot);
        match (self.mode, self.anchor) {
            (Mode::Rectangle, Some(a)) => {
                let before = self.decor();
                self.sel = Rect::from_points(a, (x, y)).intersect(&self.shot.bounds());
                self.invalidate(before.union(&self.decor()).union(&self.label_room()));
            }
            (Mode::Freeform, Some(_)) => {
                let last = *self.lasso.last().unwrap();
                if last != (x, y) {
                    self.lasso.push((x, y));
                    let seg = Rect::from_points(last, (x, y)).inflate(self.px(3.0));
                    self.invalidate(seg);
                }
            }
            (Mode::Window | Mode::Full, _) if hot.is_none() => self.update_hover(x, y),
            _ => {}
        }
    }

    fn snip(&self, r: Rect) -> Outcome {
        Outcome::Snip(self.shot.dib.region(r), r.offset(self.shot.origin.0, self.shot.origin.1))
    }

    fn lasso_image(&self) -> Option<(Image, Rect)> {
        if self.lasso.len() < 3 {
            return None;
        }
        let mut b = Rect::new(i32::MAX, i32::MAX, i32::MIN, i32::MIN);
        for &(x, y) in &self.lasso {
            b = Rect::new(b.l.min(x), b.t.min(y), b.r.max(x + 1), b.b.max(y + 1));
        }
        let b = b.intersect(&self.shot.bounds());
        if b.w() < 2 || b.h() < 2 {
            return None;
        }
        let mut img = self.shot.dib.region(b);
        let poly: Vec<(f32, f32)> =
            self.lasso.iter().map(|&(x, y)| ((x - b.l) as f32 + 0.5, (y - b.t) as f32 + 0.5)).collect();
        img.mask_polygon(&poly);
        Some((img, b))
    }

    /// Mouse release: returns the snip when the gesture produced one.
    fn mouse_up(&mut self, x: i32, y: i32) -> Option<Outcome> {
        if let Some(i) = self.pressed.take() {
            self.invalidate(self.bar);
            if self.button_at(x, y) != Some(i) {
                return None;
            }
            return match BUTTONS[i].0 {
                Button::Mode(m) => {
                    self.set_mode(m);
                    None
                }
                Button::FullScreen => Some(self.snip(self.shot.bounds())),
                Button::Close => Some(Outcome::Cancel),
            };
        }
        let anchor = self.anchor.take()?;
        unsafe {
            let _ = ReleaseCapture();
        }
        match self.mode {
            Mode::Rectangle => {
                let sel = self.sel;
                if sel.w() >= 2 && sel.h() >= 2 {
                    return Some(self.snip(sel));
                }
                self.sel = Rect::default();
                self.invalidate(self.decor().union(&self.bar).union(&Rect::from_points(anchor, (x, y)).inflate(4)));
                None
            }
            Mode::Freeform => {
                let img = self.lasso_image();
                if img.is_none() {
                    let mut b = Rect::default();
                    for &(x, y) in &self.lasso {
                        b = b.union(&Rect::new(x, y, x + 1, y + 1));
                    }
                    self.lasso.clear();
                    self.invalidate(b.inflate(self.px(3.0)).union(&self.bar));
                }
                img.map(|(img, b)| Outcome::Snip(img, b.offset(self.shot.origin.0, self.shot.origin.1)))
            }
            Mode::Window | Mode::Full => {
                let r = self.hover.or_else(|| self.shot.window_at(x, y))?;
                Some(self.snip(r))
            }
        }
    }

    fn mouse_down(&mut self, x: i32, y: i32) {
        if let Some(i) = self.button_at(x, y) {
            self.pressed = Some(i);
            self.invalidate(self.bar);
            return;
        }
        match self.mode {
            Mode::Rectangle => {
                self.anchor = Some((x, y));
                self.sel = Rect::default();
                self.invalidate(self.bar);
            }
            Mode::Freeform => {
                self.anchor = Some((x, y));
                self.lasso = vec![(x, y)];
                self.invalidate(self.bar);
            }
            Mode::Window | Mode::Full => {
                self.anchor = Some((x, y));
                self.update_hover(x, y);
            }
        }
        unsafe {
            SetCapture(self.hwnd);
        }
    }
}

impl Drop for Overlay {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(self.text_font.into());
        }
    }
}

fn finish(outcome: Outcome) {
    let Some(ov) = OVERLAY.with(|c| c.try_borrow_mut().ok().and_then(|mut g| g.take())) else { return };
    let hwnd = ov.hwnd;
    let mode = ov.mode;
    unsafe {
        let _ = UnregisterHotKey(Some(hwnd), ESC_HOTKEY);
        let _ = DestroyWindow(hwnd);
    }
    // the screen copies are the biggest allocation dvsnip makes; free them before handing the snip on
    drop(ov);
    if let Some(done) = DONE.with(|d| d.borrow_mut().take()) {
        done(outcome, mode);
    }
}

extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_PAINT => {
                if with(|o| o.paint()).is_none() {
                    let mut ps = PAINTSTRUCT::default();
                    BeginPaint(hwnd, &mut ps);
                    let _ = EndPaint(hwnd, &ps);
                }
                LRESULT(0)
            }
            WM_ERASEBKGND => LRESULT(1),
            WM_SETCURSOR => {
                let over_bar = with(|o| {
                    let mut p = POINT::default();
                    let _ = GetCursorPos(&mut p);
                    let _ = ScreenToClient(hwnd, &mut p);
                    o.button_at(p.x, p.y).is_some()
                })
                .unwrap_or(false);
                SetCursor(LoadCursorW(None, if over_bar { IDC_ARROW } else { IDC_CROSS }).ok());
                LRESULT(1)
            }
            WM_MOUSEMOVE => {
                let (x, y) = point(lp.0);
                with(|o| o.mouse_move(x, y));
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                let (x, y) = point(lp.0);
                with(|o| o.mouse_down(x, y));
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                let (x, y) = point(lp.0);
                if let Some(out) = with(|o| o.mouse_up(x, y)).flatten() {
                    finish(out);
                }
                LRESULT(0)
            }
            WM_RBUTTONUP => {
                finish(Outcome::Cancel);
                LRESULT(0)
            }
            WM_KEYDOWN if wp.0 == VK_ESCAPE.0 as usize => {
                finish(Outcome::Cancel);
                LRESULT(0)
            }
            WM_HOTKEY if wp.0 == ESC_HOTKEY as usize => {
                finish(Outcome::Cancel);
                LRESULT(0)
            }
            WM_ACTIVATE => {
                let active = sys::loword(wp.0) != 0;
                let lost = with(|o| {
                    let lost = o.was_active && !active;
                    o.was_active |= active;
                    lost
                });
                if lost == Some(true) {
                    finish(Outcome::Cancel);
                }
                LRESULT(0)
            }
            // the overlay spans every monitor in physical pixels; it must not be resized for any one DPI
            WM_DPICHANGED => LRESULT(0),
            WM_DISPLAYCHANGE => {
                finish(Outcome::Cancel);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

fn register_class() -> PCWSTR {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    let name = w!("dvsnipOverlay");
    ONCE.call_once(|| unsafe {
        let hinst = windows::Win32::System::LibraryLoader::GetModuleHandleW(None).unwrap_or_default();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(proc),
            hInstance: hinst.into(),
            hCursor: LoadCursorW(None, IDC_CROSS).unwrap_or_default(),
            lpszClassName: name,
            ..Default::default()
        };
        RegisterClassExW(&wc);
    });
    name
}

extern "system" fn dim_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

/// The instant stand-in for the overlay: a black window at half opacity over the whole desktop, left out of screen
/// captures so the copy taken behind it is clean. It does not take focus, so open menus stay open to be captured.
/// None where Windows cannot exclude a window from capture (before Windows 10 2004); the screen is then copied
/// before anything is shown.
fn dim_layer() -> Option<HWND> {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    let class = w!("dvsnipDim");
    unsafe {
        let hinst = windows::Win32::System::LibraryLoader::GetModuleHandleW(None).unwrap_or_default();
        ONCE.call_once(|| {
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(dim_proc),
                hInstance: hinst.into(),
                hCursor: LoadCursorW(None, IDC_CROSS).unwrap_or_default(),
                hbrBackground: HBRUSH(GetStockObject(BLACK_BRUSH).0),
                lpszClassName: class,
                ..Default::default()
            };
            RegisterClassExW(&wc);
        });
        let s = capture::virtual_screen();
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED | WS_EX_NOACTIVATE,
            class,
            w!(""),
            WS_POPUP,
            s.l,
            s.t,
            s.w(),
            s.h(),
            None,
            None,
            Some(hinst.into()),
            None,
        )
        .ok()?;
        if SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE).is_err() {
            let _ = DestroyWindow(hwnd);
            return None;
        }
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 128, LWA_ALPHA);
        let on: i32 = 1;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_TRANSITIONS_FORCEDISABLED, &on as *const i32 as *const _, 4);
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        let _ = UpdateWindow(hwnd);
        Some(hwnd)
    }
}

fn destroy(layer: Option<HWND>) {
    if let Some(h) = layer {
        unsafe {
            let _ = DestroyWindow(h);
        }
    }
}

fn monitor_scale(x: i32, y: i32) -> f32 {
    unsafe {
        let m = MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST);
        let (mut dx, mut dy) = (96u32, 96u32);
        let _ = GetDpiForMonitor(m, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
        dx as f32 / 96.0
    }
}

/// Freezes the screen and shows the overlay. `done` runs once with the snip (or a cancel) and the mode the overlay
/// was left in.
pub fn start(mode: Mode, dark: bool, done: Box<dyn FnOnce(Outcome, Mode)>) -> bool {
    if active() {
        sys::log("overlay: already open");
        return false;
    }
    let t0 = std::time::Instant::now();
    let layer = dim_layer();
    let have_layer = layer.is_some();
    let t_layer = t0.elapsed();
    // the screen is copied on another thread while this one builds the window
    let Some(pending) = capture::begin_grab() else {
        sys::log("overlay: screen capture failed");
        destroy(layer);
        return false;
    };
    let class = register_class();
    let mut cur = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut cur);
    }
    let scale = monitor_scale(cur.x, cur.y);
    unsafe {
        let hinst = windows::Win32::System::LibraryLoader::GetModuleHandleW(None).unwrap_or_default();
        let Ok(hwnd) = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            class,
            w!("dvsnip"),
            WS_POPUP,
            pending.origin.0,
            pending.origin.1,
            pending.size().0,
            pending.size().1,
            None,
            None,
            Some(hinst.into()),
            None,
        ) else {
            let _ = pending.finish();
            destroy(layer);
            return false;
        };
        let on: i32 = 1;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_TRANSITIONS_FORCEDISABLED, &on as *const i32 as *const _, 4);
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_CLOAK, &on as *const i32 as *const _, 4);
        // shown now, cloaked and left out of screen captures, so the slow part of showing a window happens while the
        // screen is being copied; the first paint and the reveal come after
        let _ = SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE);
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        let text_font = font(w!("Segoe UI"), 12.0 * scale, 400);
        let t_made = t0.elapsed();
        let Some(shot) = pending.finish() else {
            sys::log("overlay: screen capture failed");
            let _ = DestroyWindow(hwnd);
            destroy(layer);
            return false;
        };
        let t_grab = t0.elapsed();
        let mut ov = Overlay {
            hwnd,
            shot,
            strip: None,
            mode: if mode == Mode::Full { Mode::Rectangle } else { mode },
            anchor: None,
            sel: Rect::default(),
            lasso: Vec::new(),
            hover: None,
            label: Rect::default(),
            bar: Rect::default(),
            bar_dib: None,
            hot: None,
            pressed: None,
            scale,
            text_font,
            colors: colors(dark),
            was_active: false,
        };
        ov.layout_bar();
        let (cx, cy) = (cur.x - ov.shot.origin.0, cur.y - ov.shot.origin.1);
        if ov.mode == Mode::Window {
            ov.hover = ov.shot.window_at(cx, cy);
        }
        OVERLAY.with(|c| *c.borrow_mut() = Some(ov));
        DONE.with(|d| *d.borrow_mut() = Some(done));
        // drawn while cloaked, then revealed in one piece (anything painted before the overlay existed is redone)
        let _ = InvalidateRect(Some(hwnd), None, false);
        let _ = UpdateWindow(hwnd);
        let off: i32 = 0;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_CLOAK, &off as *const i32 as *const _, 4);
        // the overlay now covers the black layer with the same picture
        destroy(layer);
        let _ = SetForegroundWindow(hwnd);
        // Esc works even if Windows refused to bring the overlay to the foreground
        let _ = RegisterHotKey(Some(hwnd), ESC_HOTKEY, HOT_KEY_MODIFIERS(0), VK_ESCAPE.0 as u32);
        sys::log(&format!(
            "overlay: dim layer {} at {:.1} ms, window ready {:.1} ms, capture done {:.1} ms, overlay shown {:.1} ms",
            if have_layer { "shown" } else { "unavailable" },
            t_layer.as_secs_f64() * 1000.0,
            t_made.as_secs_f64() * 1000.0,
            t_grab.as_secs_f64() * 1000.0,
            t0.elapsed().as_secs_f64() * 1000.0
        ));
    }
    true
}

/// Ends an open overlay without a snip.
pub fn cancel() {
    if active() {
        finish(Outcome::Cancel);
    }
}
