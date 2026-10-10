// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvsnip contributors
// Pixel buffers. No Windows dependencies, so everything here is unit-tested.

/// An integer rectangle, right and bottom exclusive.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Rect {
    pub l: i32,
    pub t: i32,
    pub r: i32,
    pub b: i32,
}

impl Rect {
    pub fn new(l: i32, t: i32, r: i32, b: i32) -> Rect {
        Rect { l, t, r, b }
    }

    pub fn from_points(a: (i32, i32), b: (i32, i32)) -> Rect {
        Rect { l: a.0.min(b.0), t: a.1.min(b.1), r: a.0.max(b.0), b: a.1.max(b.1) }
    }

    pub fn w(&self) -> i32 {
        self.r - self.l
    }

    pub fn h(&self) -> i32 {
        self.b - self.t
    }

    pub fn is_empty(&self) -> bool {
        self.r <= self.l || self.b <= self.t
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.l && x < self.r && y >= self.t && y < self.b
    }

    pub fn intersect(&self, o: &Rect) -> Rect {
        Rect { l: self.l.max(o.l), t: self.t.max(o.t), r: self.r.min(o.r), b: self.b.min(o.b) }
    }

    pub fn union(&self, o: &Rect) -> Rect {
        if self.is_empty() {
            return *o;
        }
        if o.is_empty() {
            return *self;
        }
        Rect { l: self.l.min(o.l), t: self.t.min(o.t), r: self.r.max(o.r), b: self.b.max(o.b) }
    }

    pub fn inflate(&self, d: i32) -> Rect {
        Rect { l: self.l - d, t: self.t - d, r: self.r + d, b: self.b + d }
    }

    pub fn offset(&self, dx: i32, dy: i32) -> Rect {
        Rect { l: self.l + dx, t: self.t + dy, r: self.r + dx, b: self.b + dy }
    }
}

/// Premultiplied BGRA pixels, one `u32` each (0xAARRGGBB), top-down rows with no padding.
#[derive(Clone)]
pub struct Image {
    pub w: u32,
    pub h: u32,
    pub px: Vec<u32>,
}

impl Image {
    pub fn bounds(&self) -> Rect {
        Rect::new(0, 0, self.w as i32, self.h as i32)
    }

    /// Copies `r` (clipped to the source) out of a row-major buffer `stride` pixels wide, forcing alpha to opaque:
    /// a screen capture leaves the alpha byte undefined.
    pub fn from_region(src: &[u32], stride: usize, rows: usize, r: Rect) -> Image {
        let r = r.intersect(&Rect::new(0, 0, stride as i32, rows as i32));
        if r.is_empty() {
            return Image { w: 0, h: 0, px: Vec::new() };
        }
        let (w, h) = (r.w() as usize, r.h() as usize);
        let mut px = Vec::with_capacity(w * h);
        for y in r.t as usize..r.b as usize {
            let row = &src[y * stride + r.l as usize..y * stride + r.r as usize];
            px.extend(row.iter().map(|&p| p | 0xFF00_0000));
        }
        Image { w: w as u32, h: h as u32, px }
    }

    pub fn crop(&self, r: Rect) -> Image {
        let r = r.intersect(&self.bounds());
        if r == self.bounds() {
            return self.clone();
        }
        let mut px = Vec::with_capacity((r.w().max(0) * r.h().max(0)) as usize);
        for y in r.t..r.b {
            let s = (y as u32 * self.w) as usize;
            px.extend_from_slice(&self.px[s + r.l as usize..s + r.r as usize]);
        }
        Image { w: r.w().max(0) as u32, h: r.h().max(0) as u32, px }
    }

    pub fn is_opaque(&self) -> bool {
        self.px.iter().all(|&p| p >> 24 == 0xFF)
    }

    /// Clears every pixel outside the polygon (image coordinates, nonzero winding) to transparent.
    pub fn mask_polygon(&mut self, poly: &[(f32, f32)]) {
        let w = self.w as usize;
        for (y, row) in self.px.chunks_mut(w.max(1)).enumerate() {
            let spans = polygon_spans(poly, y as f32 + 0.5);
            let mut x = 0usize;
            for (a, b) in spans {
                let a = (a.round().max(0.0) as usize).min(w);
                let b = (b.round().max(0.0) as usize).min(w);
                row[x..a.max(x)].fill(0);
                x = x.max(b);
            }
            row[x.min(w)..].fill(0);
        }
    }
}

/// Inside spans of a polygon along the horizontal line `y`, sorted, using the nonzero winding rule.
pub fn polygon_spans(poly: &[(f32, f32)], y: f32) -> Vec<(f32, f32)> {
    let n = poly.len();
    let mut hits: Vec<(f32, i32)> = Vec::new();
    for i in 0..n {
        let (a, b) = (poly[i], poly[(i + 1) % n]);
        if (a.1 <= y && b.1 > y) || (b.1 <= y && a.1 > y) {
            let x = a.0 + (y - a.1) / (b.1 - a.1) * (b.0 - a.0);
            hits.push((x, if b.1 > a.1 { 1 } else { -1 }));
        }
    }
    hits.sort_by(|p, q| p.0.total_cmp(&q.0));
    let mut spans = Vec::new();
    let mut wind = 0;
    let mut start = 0.0;
    for (x, d) in hits {
        let was = wind != 0;
        wind += d;
        if !was && wind != 0 {
            start = x;
        } else if was && wind == 0 {
            spans.push((start, x));
        }
    }
    spans
}

/// Shrinks the image so its longer side is at most `max` pixels, each output pixel the average of the source
/// pixels it covers (a box filter, which keeps small text readable). Returns a copy when it already fits.
pub fn fit_within(img: &Image, max: u32) -> Image {
    let long = img.w.max(img.h);
    if max == 0 || long <= max {
        return img.clone();
    }
    let nw = ((img.w as u64 * max as u64 + long as u64 / 2) / long as u64).max(1) as u32;
    let nh = ((img.h as u64 * max as u64 + long as u64 / 2) / long as u64).max(1) as u32;
    let span = |i: u32, n: u32, src: u32| {
        let a = (i as u64 * src as u64 / n as u64) as usize;
        let b = (((i + 1) as u64 * src as u64 / n as u64) as usize).max(a + 1);
        (a, b)
    };
    let cols: Vec<(usize, usize)> = (0..nw).map(|x| span(x, nw, img.w)).collect();
    let mut px = Vec::with_capacity((nw * nh) as usize);
    for y in 0..nh {
        let (y0, y1) = span(y, nh, img.h);
        for &(x0, x1) in &cols {
            let mut sum = [0u32; 4];
            for row in y0..y1 {
                for &p in &img.px[row * img.w as usize + x0..row * img.w as usize + x1] {
                    for (c, s) in sum.iter_mut().enumerate() {
                        *s += (p >> (c * 8)) & 0xFF;
                    }
                }
            }
            let n = ((x1 - x0) * (y1 - y0)) as u32;
            px.push(sum.iter().enumerate().fold(0u32, |acc, (c, s)| acc | ((s + n / 2) / n) << (c * 8)));
        }
    }
    Image { w: nw, h: nh, px }
}

/// Straight (not premultiplied) BGRA, as image file encoders expect.
pub fn unpremultiply(px: &[u32]) -> Vec<u32> {
    px.iter()
        .map(|&p| {
            let a = p >> 24;
            if a == 0xFF || a == 0 {
                return p;
            }
            let ch = |s: u32| (((p >> s) & 0xFF) * 255 + a / 2) / a;
            (a << 24) | (ch(16).min(255) << 16) | (ch(8).min(255) << 8) | ch(0).min(255)
        })
        .collect()
}

/// A bottom-up 32-bit `CF_DIB` (header plus pixels) for the clipboard. Many programs ignore the alpha byte, so
/// transparent pixels (a freeform snip's outside) are composited onto white.
pub fn dib(img: &Image) -> Vec<u8> {
    let mut out = Vec::with_capacity(40 + img.px.len() * 4);
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(img.w as i32).to_le_bytes());
    out.extend_from_slice(&(img.h as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
    out.extend_from_slice(&((img.px.len() * 4) as u32).to_le_bytes());
    out.extend_from_slice(&[0; 16]);
    let w = img.w as usize;
    for row in img.px.chunks(w.max(1)).rev() {
        for &p in row {
            let a = p >> 24;
            let q = if a == 0xFF {
                p
            } else {
                // premultiplied over white: c + (255 - a)
                let k = 255 - a;
                let ch = |s: u32| (((p >> s) & 0xFF) + k).min(255);
                0xFF00_0000 | (ch(16) << 16) | (ch(8) << 8) | ch(0)
            };
            out.extend_from_slice(&q.to_le_bytes());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(w: u32, h: u32) -> Image {
        Image { w, h, px: (0..w * h).map(|i| 0xFF00_0000 | i).collect() }
    }

    #[test]
    fn region_and_crop() {
        let src: Vec<u32> = (0..100).collect();
        let a = Image::from_region(&src, 10, 10, Rect::new(2, 3, 5, 5));
        assert_eq!((a.w, a.h), (3, 2));
        assert_eq!(a.px, vec![0xFF00_0020, 0xFF00_0021, 0xFF00_0022, 0xFF00_002A, 0xFF00_002B, 0xFF00_002C]);
        let clipped = Image::from_region(&src, 10, 10, Rect::new(8, 8, 20, 20));
        assert_eq!((clipped.w, clipped.h), (2, 2));
        let c = img(4, 4).crop(Rect::new(1, 1, 3, 4));
        assert_eq!((c.w, c.h), (2, 3));
        assert_eq!(c.px[0] & 0xFFFF, 5);
        assert_eq!(c.px[5] & 0xFFFF, 14);
    }

    #[test]
    fn triangle_mask() {
        let mut m = Image { w: 10, h: 10, px: vec![0xFFFF_FFFF; 100] };
        m.mask_polygon(&[(0.0, 0.0), (10.0, 0.0), (0.0, 10.0)]);
        // the upper-left half stays, the lower-right half is cleared
        assert_eq!(m.px[0], 0xFFFF_FFFF);
        assert_eq!(m.px[9 * 10 + 9], 0);
        assert_eq!(m.px[5 * 10 + 1], 0xFFFF_FFFF);
        assert_eq!(m.px[5 * 10 + 8], 0);
        assert!(!m.is_opaque());
    }

    #[test]
    fn self_crossing_lasso_uses_nonzero() {
        // a figure drawn twice around the same square still counts as inside
        let sq = [(1.0, 1.0), (9.0, 1.0), (9.0, 9.0), (1.0, 9.0)];
        let twice: Vec<_> = sq.iter().chain(sq.iter()).copied().collect();
        assert_eq!(polygon_spans(&twice, 5.0), vec![(1.0, 9.0)]);
    }

    #[test]
    fn dib_is_bottom_up_and_opaque() {
        let a = img(3, 2);
        let d = dib(&a);
        assert_eq!(d.len(), 40 + 6 * 4);
        // bottom-up: the first row written is the last image row
        assert_eq!(&d[40..44], &a.px[3].to_le_bytes());
        // transparent pixels become white on the clipboard
        let t = Image { w: 1, h: 1, px: vec![0] };
        assert_eq!(&dib(&t)[40..44], &0xFFFF_FFFFu32.to_le_bytes());
    }

    #[test]
    fn fit_within_averages() {
        let big = Image {
            w: 4,
            h: 2,
            px: vec![
                0xFF00_0000,
                0xFF00_00FF,
                0xFFFF_FFFF,
                0xFFFF_FFFF,
                0xFF00_0000,
                0xFF00_00FF,
                0xFF00_0000,
                0xFF00_0000,
            ],
        };
        let s = fit_within(&big, 2);
        assert_eq!((s.w, s.h), (2, 1));
        assert_eq!(s.px, vec![0xFF00_0080, 0xFF80_8080]);
        assert_eq!(fit_within(&big, 8).px, big.px);
        let tall = Image { w: 3, h: 3000, px: vec![0xFF10_2030; 9000] };
        let t = fit_within(&tall, 1000);
        assert_eq!((t.w, t.h), (1, 1000));
        assert!(t.px.iter().all(|&p| p == 0xFF10_2030));
    }

    #[test]
    fn unpremultiply_half() {
        assert_eq!(unpremultiply(&[0x8040_0000])[0], 0x8080_0000);
        assert_eq!(unpremultiply(&[0xFF12_3456])[0], 0xFF12_3456);
    }
}
