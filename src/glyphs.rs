// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvsnip contributors
// The overlay toolbar icons, drawn by dvsnip itself (like the app icon in icon_art.rs) instead of taken from an icon
// font: a few lines on a 16-unit grid, rasterized to an antialiased coverage mask at the size needed.
// No Windows dependencies.

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Icon {
    RectSnip,
    WindowSnip,
    FullSnip,
    FreeSnip,
    Close,
}

pub const ALL: [Icon; 5] = [Icon::RectSnip, Icon::WindowSnip, Icon::FullSnip, Icon::FreeSnip, Icon::Close];

type P = [f32; 2];

/// Lines to stroke (open or closed polylines), in grid units.
#[derive(Default)]
struct Shape {
    lines: Vec<(Vec<P>, bool)>,
}

impl Shape {
    fn open(&mut self, pts: &[P]) -> &mut Self {
        self.lines.push((pts.to_vec(), false));
        self
    }

    fn closed(&mut self, pts: &[P]) -> &mut Self {
        self.lines.push((pts.to_vec(), true));
        self
    }

    fn line(&mut self, a: P, b: P) -> &mut Self {
        self.open(&[a, b])
    }

    fn round_rect(&mut self, l: f32, t: f32, r: f32, b: f32, rad: f32) -> &mut Self {
        let pts = round_rect(l, t, r, b, rad);
        self.closed(&pts)
    }

    /// A polyline cut into dashes `on` long with gaps `off` long.
    fn dashed(&mut self, pts: &[P], closed: bool, on: f32, off: f32) -> &mut Self {
        let mut path = pts.to_vec();
        if closed {
            path.push(pts[0]);
        }
        let mut dash: Vec<P> = vec![path[0]];
        let (mut drawing, mut left) = (true, on);
        for w in path.windows(2) {
            let (mut a, b) = (w[0], w[1]);
            let mut seg = dist(a, b);
            while seg > 0.0 {
                let step = seg.min(left);
                let t = step / seg;
                let p = [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t];
                if drawing {
                    dash.push(p);
                }
                seg -= step;
                left -= step;
                a = p;
                if left <= 1e-4 {
                    if drawing && dash.len() > 1 {
                        self.lines.push((std::mem::take(&mut dash), false));
                    }
                    drawing = !drawing;
                    left = if drawing { on } else { off };
                    dash = vec![p];
                }
            }
        }
        if drawing && dash.len() > 1 {
            self.lines.push((dash, false));
        }
        self
    }
}

fn dist(a: P, b: P) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}

/// Points along a circular arc; angles in degrees, clockwise on screen (y grows downwards).
fn arc(c: P, r: f32, from: f32, to: f32) -> Vec<P> {
    let n = (((to - from).abs() / 12.0).ceil() as usize).max(2);
    (0..=n)
        .map(|i| {
            let a = (from + (to - from) * i as f32 / n as f32).to_radians();
            [c[0] + r * a.cos(), c[1] + r * a.sin()]
        })
        .collect()
}

fn round_rect(l: f32, t: f32, r: f32, b: f32, rad: f32) -> Vec<P> {
    let mut v = Vec::new();
    v.extend(arc([r - rad, t + rad], rad, -90.0, 0.0));
    v.extend(arc([r - rad, b - rad], rad, 0.0, 90.0));
    v.extend(arc([l + rad, b - rad], rad, 90.0, 180.0));
    v.extend(arc([l + rad, t + rad], rad, 180.0, 270.0));
    v
}

fn shape(icon: Icon) -> Shape {
    let mut s = Shape::default();
    match icon {
        Icon::RectSnip => {
            let pts = round_rect(2.0, 3.0, 14.0, 13.0, 1.5);
            s.dashed(&pts, true, 2.4, 1.4);
        }
        Icon::WindowSnip => {
            s.round_rect(2.0, 3.0, 14.0, 13.0, 1.5).line([2.0, 6.0], [14.0, 6.0]);
        }
        Icon::FullSnip => {
            s.round_rect(1.5, 3.0, 14.5, 13.0, 1.5)
                .open(&[[4.0, 7.5], [4.0, 5.5], [6.0, 5.5]])
                .open(&[[10.0, 5.5], [12.0, 5.5], [12.0, 7.5]])
                .open(&[[12.0, 8.5], [12.0, 10.5], [10.0, 10.5]])
                .open(&[[6.0, 10.5], [4.0, 10.5], [4.0, 8.5]]);
        }
        Icon::FreeSnip => {
            let blob: Vec<P> = (0..36)
                .map(|i| {
                    let a = i as f32 / 36.0 * std::f32::consts::TAU;
                    let r = 5.0 + 0.9 * (3.0 * a + 0.6).sin();
                    [8.0 + r * a.cos(), 7.8 + r * 0.92 * a.sin()]
                })
                .collect();
            s.dashed(&blob, true, 2.4, 1.4);
        }
        Icon::Close => {
            s.line([3.5, 3.5], [12.5, 12.5]).line([12.5, 3.5], [3.5, 12.5]);
        }
    }
    s
}

fn seg_dist(p: P, a: P, b: P) -> f32 {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let len = dx * dx + dy * dy;
    let t = if len == 0.0 { 0.0 } else { (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / len).clamp(0.0, 1.0) };
    dist(p, [a[0] + t * dx, a[1] + t * dy])
}

/// Coverage (0..=255) of the icon drawn `px` pixels square, row by row.
pub fn mask(icon: Icon, px: u32) -> Vec<u8> {
    let s = shape(icon);
    let k = px as f32 / 16.0;
    // about 1 px of line at 16 px, thickening gently with the size
    let half = (1.15 * k).max(1.0) / 2.0;
    let mut segs: Vec<(P, P)> = Vec::new();
    for (pts, closed) in &s.lines {
        let scaled: Vec<P> = pts.iter().map(|p| [p[0] * k, p[1] * k]).collect();
        for w in scaled.windows(2) {
            segs.push((w[0], w[1]));
        }
        if *closed && scaled.len() > 2 {
            segs.push((*scaled.last().unwrap(), scaled[0]));
        }
    }
    let mut out = vec![0u8; (px * px) as usize];
    for y in 0..px {
        for x in 0..px {
            let p = [x as f32 + 0.5, y as f32 + 0.5];
            let d = segs.iter().map(|(a, b)| seg_dist(p, *a, *b)).fold(f32::MAX, f32::min);
            let cov = (half - d + 0.5).clamp(0.0, 1.0);
            out[(y * px + x) as usize] = (cov * 255.0).round() as u8;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_draws_inside_its_box() {
        for icon in ALL {
            let m = mask(icon, 20);
            let ink: u32 = m.iter().map(|&a| a as u32).sum();
            assert!(ink > 255 * 12, "{icon:?} is nearly empty");
            // nothing touches the outermost pixel ring, so icons never look clipped
            for i in 0..20 {
                for (x, y) in [(i, 0), (i, 19), (0, i), (19, i)] {
                    assert!(m[y * 20 + x] < 128, "{icon:?} reaches the edge at {x},{y}");
                }
            }
        }
    }

    #[test]
    fn dashes_leave_gaps() {
        let mut s = Shape::default();
        s.dashed(&[[0.0, 0.0], [10.0, 0.0]], false, 2.0, 1.0);
        assert_eq!(s.lines.len(), 4);
        assert!((s.lines[1].0[0][0] - 3.0).abs() < 1e-4);
    }

    /// Writes a contact sheet of every icon (light and dark, 20 and 40 px) and the app icon to DVSNIP_SHEET.
    #[test]
    #[ignore]
    fn contact_sheet() {
        let Some(path) = std::env::var_os("DVSNIP_SHEET") else { return };
        let cell = 48u32;
        let (w, h) = (cell * ALL.len() as u32, cell * 4 + 256);
        let mut img = crate::image::Image { w, h, px: vec![0xFFFF_FFFF; (w * h) as usize] };
        for (row, (size, bg, fg)) in
            [(20, 0xFFFFFF, 0x1B1B1B), (40, 0xFFFFFF, 0x1B1B1B), (20, 0x202020, 0xFFFFFF), (40, 0x202020, 0xFFFFFF)]
                .into_iter()
                .enumerate()
        {
            for (i, icon) in ALL.iter().enumerate() {
                let m = mask(*icon, size);
                for y in 0..cell {
                    for x in 0..cell {
                        let (ix, iy) = (x as i32 - ((cell - size) / 2) as i32, y as i32 - ((cell - size) / 2) as i32);
                        let a = if ix >= 0 && iy >= 0 && (ix as u32) < size && (iy as u32) < size {
                            m[(iy as u32 * size + ix as u32) as usize] as u32
                        } else {
                            0
                        };
                        let mix = |s: u32| (((bg >> s) & 0xFF) * (255 - a) + ((fg >> s) & 0xFF) * a) / 255;
                        img.px[((row as u32 * cell + y) * w + i as u32 * cell + x) as usize] =
                            0xFF00_0000 | mix(16) << 16 | mix(8) << 8 | mix(0);
                    }
                }
            }
        }
        // the app icon at 256, 48, 32 and 16 px on a checkerboard
        let mut x0 = 0;
        for size in [256u32, 48, 32, 16] {
            let rgba = crate::icon_art::rgba(size);
            for y in 0..size {
                for x in 0..size {
                    let i = ((y * size + x) * 4) as usize;
                    let a = rgba[i + 3] as u32;
                    let bg = if ((x / 8) + (y / 8)) % 2 == 0 { 0xCC } else { 0x99 };
                    let ch = |c: u8| (c as u32 * a + bg * (255 - a)) / 255;
                    img.px[((cell * 4 + y) * w + x0 + x) as usize] =
                        0xFF00_0000 | ch(rgba[i]) << 16 | ch(rgba[i + 1]) << 8 | ch(rgba[i + 2]);
                }
            }
            x0 += size + 16;
        }
        crate::png::save(&img, std::path::Path::new(&path)).unwrap();
    }
}
