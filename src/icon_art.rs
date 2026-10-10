// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvsnip contributors
fn inside_round_rect(x: f32, y: f32, x0: f32, y0: f32, x1: f32, y1: f32, r: f32) -> bool {
    if x < x0 || x > x1 || y < y0 || y > y1 {
        return false;
    }
    let cx = x.clamp(x0 + r, (x1 - r).max(x0 + r));
    let cy = y.clamp(y0 + r, (y1 - r).max(y0 + r));
    (x - cx).powi(2) + (y - cy).powi(2) <= r * r
}

fn over(dst: &mut [f32; 4], src: [f32; 4]) {
    let a = src[3] + dst[3] * (1.0 - src[3]);
    if a > 0.0 {
        for i in 0..3 {
            dst[i] = (src[i] * src[3] + dst[i] * dst[3] * (1.0 - src[3])) / a;
        }
    }
    dst[3] = a;
}

fn mix(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

fn seg_dist(x: f32, y: f32, ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
    let (dx, dy) = (bx - ax, by - ay);
    let t = (((x - ax) * dx + (y - ay) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
    ((x - ax - t * dx).powi(2) + (y - ay - t * dy).powi(2)).sqrt()
}

// a dashed selection frame with a pair of scissors over its corner, on a transparent background
fn sample(x: f32, y: f32, size: f32) -> [f32; 4] {
    let mut c = [0.0f32; 4];
    let line = (0.065f32).max(1.6 / size);
    let (x0, y0, x1, y1) = (0.08, 0.08, 0.78, 0.78);
    let outer = inside_round_rect(x, y, x0, y0, x1, y1, 0.09);
    let inner = inside_round_rect(x, y, x0 + line, y0 + line, x1 - line, y1 - line, 0.09 - line * 0.6);
    if inner {
        let p = mix([1.0, 1.0, 1.0], [0.90, 0.93, 0.97], ((y - y0) / (y1 - y0)).clamp(0.0, 1.0));
        over(&mut c, [p[0], p[1], p[2], 0.95]);
    }
    if outer && !inner {
        // dashes along the frame; small sizes keep a solid frame so it stays readable
        let along = if (y - y0) < line * 1.5 || (y1 - y) < line * 1.5 { x } else { y };
        let gap = size >= 32.0 && ((along - x0 - 0.02) / 0.14).rem_euclid(1.0) > 0.64;
        if !gap {
            over(&mut c, [0.91, 0.30, 0.24, 1.0]);
        }
    }
    // open scissors: each blade runs from its tip through the pivot to the ring handle on the far side
    let (px, py) = (0.58, 0.58);
    let blade = (0.055f32).max(1.3 / size);
    let shank = (0.03f32).max(1.0 / size);
    for (tx, ty, hx, hy) in [(0.42, 0.13, 0.70, 0.84), (0.13, 0.42, 0.84, 0.70)] {
        let reach = ((x - tx).powi(2) + (y - ty).powi(2)).sqrt() / 0.45;
        if seg_dist(x, y, tx, ty, px, py) < blade * (0.3 + 0.7 * reach.min(1.0)) {
            let p = mix([0.88, 0.91, 0.95], [0.48, 0.53, 0.61], ((x + y) * 0.5 - 0.2).clamp(0.0, 1.0));
            over(&mut c, [p[0], p[1], p[2], 1.0]);
        }
        // the shank stops at the ring's edge
        let (dx, dy) = (hx - px, hy - py);
        let len = (dx * dx + dy * dy).sqrt();
        let (sx, sy) = (hx - dx / len * 0.1, hy - dy / len * 0.1);
        if seg_dist(x, y, px, py, sx, sy) < shank {
            over(&mut c, [0.13, 0.45, 0.85, 1.0]);
        }
        let r = ((x - hx).powi(2) + (y - hy).powi(2)).sqrt();
        let ring = (0.045f32).max(1.5 / size);
        if r < 0.11 && r > 0.11 - ring {
            over(&mut c, [0.13, 0.45, 0.85, 1.0]);
        }
    }
    if ((x - px).powi(2) + (y - py).powi(2)).sqrt() < (0.032f32).max(0.9 / size) {
        over(&mut c, [0.25, 0.28, 0.33, 1.0]);
    }
    c
}

pub fn rgba(size: u32) -> Vec<u8> {
    const SS: u32 = 4;
    let s = size as f32;
    let mut out = Vec::with_capacity((size * size * 4) as usize);
    for py in 0..size {
        for px in 0..size {
            let (mut r, mut g, mut b, mut a) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
            for sy in 0..SS {
                for sx in 0..SS {
                    let x = (px as f32 + (sx as f32 + 0.5) / SS as f32) / s;
                    let y = (py as f32 + (sy as f32 + 0.5) / SS as f32) / s;
                    let c = sample(x, y, s);
                    r += c[0] * c[3];
                    g += c[1] * c[3];
                    b += c[2] * c[3];
                    a += c[3];
                }
            }
            let n = (SS * SS) as f32;
            let (cr, cg, cb) = if a > 0.0 { (r / a, g / a, b / a) } else { (0.0, 0.0, 0.0) };
            out.extend_from_slice(&[(cr * 255.0) as u8, (cg * 255.0) as u8, (cb * 255.0) as u8, (a / n * 255.0) as u8]);
        }
    }
    out
}

pub fn ico(sizes: &[u32]) -> Vec<u8> {
    let mut images: Vec<Vec<u8>> = Vec::new();
    for &size in sizes {
        let px = rgba(size);
        let mut d = Vec::new();
        d.extend_from_slice(&40u32.to_le_bytes()); // BITMAPINFOHEADER
        d.extend_from_slice(&(size as i32).to_le_bytes());
        d.extend_from_slice(&(size as i32 * 2).to_le_bytes()); // height covers XOR + AND masks
        d.extend_from_slice(&1u16.to_le_bytes());
        d.extend_from_slice(&32u16.to_le_bytes());
        d.extend_from_slice(&[0; 24]);
        for y in (0..size).rev() {
            for x in 0..size {
                let i = ((y * size + x) * 4) as usize;
                d.extend_from_slice(&[px[i + 2], px[i + 1], px[i], px[i + 3]]); // BGRA, bottom-up
            }
        }
        d.resize(d.len() + (((size + 31) / 32) * 4 * size) as usize, 0); // AND mask: all opaque (alpha rules)
        images.push(d);
    }
    let mut out = Vec::new();
    out.extend_from_slice(&[0, 0, 1, 0]);
    out.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * sizes.len() as u32;
    for (&size, img) in sizes.iter().zip(&images) {
        out.extend_from_slice(&[(size % 256) as u8, (size % 256) as u8, 0, 0, 1, 0, 32, 0]);
        out.extend_from_slice(&(img.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += img.len() as u32;
    }
    for img in images {
        out.extend_from_slice(&img);
    }
    out
}
