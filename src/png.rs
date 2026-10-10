// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvsnip contributors
// A small PNG encoder, so no image library or Windows codec has to load. Screenshots are mostly flat colour and
// repeated rows, which a quick deflate handles well: one hash probe per byte (LZ77 over a 32 KB window) and the fixed
// Huffman code. Each row gets the filter (none, sub, up) with the smallest sum of magnitudes, as libpng does.
// No Windows dependencies.
use crate::image::{self, Image};

static CRC_TABLE: [u32; 256] = {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
};

pub fn crc32(parts: &[&[u8]]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for p in parts {
        for &b in *p {
            c = CRC_TABLE[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8);
        }
    }
    c ^ 0xFFFF_FFFF
}

pub fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    // 5552 bytes is the most that can be summed before the 32-bit total can overflow
    for chunk in data.chunks(5552) {
        for &x in chunk {
            a += x as u32;
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    (b << 16) | a
}

struct Bits {
    out: Vec<u8>,
    acc: u64,
    n: u32,
}

impl Bits {
    /// Writes the low `len` bits of `v`, least significant first (how deflate packs everything but Huffman codes).
    #[inline]
    fn put(&mut self, v: u32, len: u32) {
        self.acc |= (v as u64) << self.n;
        self.n += len;
        while self.n >= 8 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
    }

    /// Huffman codes are defined most significant bit first.
    #[inline]
    fn code(&mut self, code: u32, len: u32) {
        self.put(code.reverse_bits() >> (32 - len), len);
    }

    fn flush(&mut self) {
        if self.n > 0 {
            self.out.push(self.acc as u8);
        }
        self.acc = 0;
        self.n = 0;
    }
}

/// The fixed Huffman code of a literal or length symbol (RFC 1951, 3.2.6).
#[inline]
fn lit_code(sym: u32) -> (u32, u32) {
    match sym {
        0..=143 => (0x30 + sym, 8),
        144..=255 => (0x190 + sym - 144, 9),
        256..=279 => (sym - 256, 7),
        _ => (0xC0 + sym - 280, 8),
    }
}

const LEN_BASE: [u16; 29] =
    [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258];
const LEN_EXTRA: [u8; 29] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145,
    8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u8; 30] =
    [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];

fn put_match(bits: &mut Bits, len: usize, dist: usize) {
    let li = LEN_BASE.partition_point(|&b| b as usize <= len) - 1;
    let (code, n) = lit_code(257 + li as u32);
    bits.code(code, n);
    bits.put((len - LEN_BASE[li] as usize) as u32, LEN_EXTRA[li] as u32);
    let di = DIST_BASE.partition_point(|&b| b as usize <= dist) - 1;
    bits.code(di as u32, 5);
    bits.put((dist - DIST_BASE[di] as usize) as u32, DIST_EXTRA[di] as u32);
}

const HASH_BITS: u32 = 15;
const WINDOW: usize = 32768;
const MAX_LEN: usize = 258;

/// A zlib stream (RFC 1950) holding `data`, compressed with LZ77 and the fixed Huffman code in one block.
pub fn zlib(data: &[u8]) -> Vec<u8> {
    let mut bits = Bits { out: Vec::with_capacity(data.len() / 4 + 64), acc: 0, n: 0 };
    bits.out.extend_from_slice(&[0x78, 0x01]);
    bits.put(1, 1); // last block
    bits.put(1, 2); // fixed Huffman
    let mut head = vec![u32::MAX; 1 << HASH_BITS];
    let hash = |i: usize| {
        let v = data[i] as u32 | (data[i + 1] as u32) << 8 | (data[i + 2] as u32) << 16;
        (v.wrapping_mul(0x9E37_79B1) >> (32 - HASH_BITS)) as usize
    };
    let mut i = 0;
    while i < data.len() {
        let mut best = 0;
        let mut dist = 0;
        if i + 3 <= data.len() {
            let h = hash(i);
            let cand = head[h];
            head[h] = i as u32;
            if cand != u32::MAX && i - cand as usize <= WINDOW {
                let c = cand as usize;
                let max = (data.len() - i).min(MAX_LEN);
                let mut l = 0;
                while l < max && data[c + l] == data[i + l] {
                    l += 1;
                }
                if l >= 3 {
                    best = l;
                    dist = i - c;
                }
            }
        }
        if best >= 3 {
            put_match(&mut bits, best, dist);
            // index the skipped positions sparsely: enough to keep finding matches, cheap on long runs
            let end = i + best;
            let mut j = i + 1;
            while j + 3 <= data.len() && j < end {
                head[hash(j)] = j as u32;
                j += if best > 32 { 8 } else { 1 };
            }
            i = end;
        } else {
            let (code, n) = lit_code(data[i] as u32);
            bits.code(code, n);
            i += 1;
        }
    }
    let (code, n) = lit_code(256);
    bits.code(code, n);
    bits.flush();
    bits.out.extend_from_slice(&adler32(data).to_be_bytes());
    bits.out
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    out.extend_from_slice(&crc32(&[kind, data]).to_be_bytes());
}

/// Filters one row of `bpp`-byte pixels with whichever of none, sub and up looks most compressible.
fn filter_row(cur: &[u8], prev: Option<&[u8]>, bpp: usize, out: &mut Vec<u8>) {
    // a byte counts as its distance from zero, so a run of small differences scores low
    let mag = |b: u8| (b as i8).unsigned_abs() as u32;
    let (head, tail) = cur.split_at(bpp.min(cur.len()));
    let sub_diff = |a: &u8, b: &u8| a.wrapping_sub(*b);
    let none: u32 = cur.iter().map(|&b| mag(b)).sum();
    let sub: u32 =
        head.iter().map(|&b| mag(b)).sum::<u32>() + tail.iter().zip(cur).map(|(a, b)| mag(sub_diff(a, b))).sum::<u32>();
    let up = prev.map_or(u32::MAX, |p| cur.iter().zip(p).map(|(a, b)| mag(sub_diff(a, b))).sum());
    if none <= sub && none <= up {
        out.push(0);
        out.extend_from_slice(cur);
    } else if sub <= up {
        out.push(1);
        out.extend_from_slice(head);
        out.extend(tail.iter().zip(cur).map(|(a, b)| sub_diff(a, b)));
    } else {
        out.push(2);
        out.extend(cur.iter().zip(prev.unwrap()).map(|(a, b)| sub_diff(a, b)));
    }
}

/// The image as PNG: 8-bit RGB when every pixel is opaque, RGBA (straight alpha) otherwise.
pub fn encode(img: &Image) -> Vec<u8> {
    let opaque = img.is_opaque();
    let bpp = if opaque { 3 } else { 4 };
    let straight;
    let px = if opaque {
        &img.px
    } else {
        straight = image::unpremultiply(&img.px);
        &straight
    };
    let w = img.w as usize;
    let mut raw = Vec::with_capacity((w * bpp + 1) * img.h as usize);
    let mut prev: Vec<u8> = Vec::new();
    let mut cur: Vec<u8> = Vec::with_capacity(w * bpp);
    for row in px.chunks_exact(w.max(1)) {
        cur.clear();
        for &p in row {
            cur.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, p as u8]);
            if !opaque {
                cur.push((p >> 24) as u8);
            }
        }
        filter_row(&cur, if prev.is_empty() { None } else { Some(&prev) }, bpp, &mut raw);
        std::mem::swap(&mut prev, &mut cur);
    }
    let mut out = Vec::with_capacity(raw.len() / 4 + 128);
    out.extend_from_slice(b"\x89PNG\r\n\x1a\n");
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&img.w.to_be_bytes());
    ihdr.extend_from_slice(&img.h.to_be_bytes());
    ihdr.extend_from_slice(&[8, if opaque { 2 } else { 6 }, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &zlib(&raw));
    chunk(&mut out, b"IEND", &[]);
    out
}

/// Writes through a temporary file, so a failed save never leaves a half-written image behind.
pub fn write_file(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension("dvsnip.tmp");
    std::fs::write(&tmp, bytes).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        e.to_string()
    })
}

pub fn save(img: &Image, path: &std::path::Path) -> Result<(), String> {
    write_file(path, &encode(img))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Just enough inflate to read back what `zlib` writes: fixed-Huffman blocks only.
    fn inflate_fixed(z: &[u8]) -> Vec<u8> {
        assert_eq!(&z[..2], &[0x78, 0x01]);
        let data = &z[2..z.len() - 4];
        let (mut pos, mut out) = (0usize, Vec::new());
        let mut bit = |n: u32| -> u32 {
            let mut v = 0;
            for k in 0..n {
                v |= (((data[pos / 8] >> (pos % 8)) & 1) as u32) << k;
                pos += 1;
            }
            v
        };
        assert_eq!(bit(1), 1);
        assert_eq!(bit(2), 1);
        // decode a fixed literal/length symbol by reading bits most significant first
        let sym = |bit: &mut dyn FnMut(u32) -> u32| -> u32 {
            let mut code = 0;
            for len in 1..=9 {
                code = (code << 1) | bit(1);
                let found = match len {
                    7 if code <= 0x17 => Some(256 + code),
                    8 if (0x30..=0xBF).contains(&code) => Some(code - 0x30),
                    8 if (0xC0..=0xC7).contains(&code) => Some(280 + code - 0xC0),
                    9 if (0x190..=0x1FF).contains(&code) => Some(144 + code - 0x190),
                    _ => None,
                };
                if let Some(s) = found {
                    return s;
                }
            }
            panic!("bad code")
        };
        loop {
            let s = sym(&mut bit);
            if s < 256 {
                out.push(s as u8);
            } else if s == 256 {
                break;
            } else {
                let li = (s - 257) as usize;
                let len = LEN_BASE[li] as usize + bit(LEN_EXTRA[li] as u32) as usize;
                let mut d = 0;
                for _ in 0..5 {
                    d = (d << 1) | bit(1);
                }
                let dist = DIST_BASE[d as usize] as usize + bit(DIST_EXTRA[d as usize] as u32) as usize;
                let start = out.len() - dist;
                for k in 0..len {
                    out.push(out[start + k]);
                }
            }
        }
        assert_eq!(adler32(&out).to_be_bytes(), z[z.len() - 4..]);
        out
    }

    #[test]
    fn checksums_match_known_values() {
        assert_eq!(crc32(&[b"123456789"]), 0xCBF4_3926);
        assert_eq!(crc32(&[b"IEND"]), 0xAE42_6082);
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
        assert_eq!(adler32(&vec![0xFF; 100_000]), adler32(&vec![0xFF; 100_000]));
    }

    #[test]
    fn deflate_round_trips() {
        let mut seed = 12345u32;
        let mut noise = || {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
            (seed >> 16) as u8
        };
        let cases: Vec<Vec<u8>> = vec![
            vec![],
            b"a".to_vec(),
            b"abcabcabcabcabcabc".to_vec(),
            vec![7; 70_000],
            (0..100_000).map(|_| noise()).collect(),
            (0..200_000u32).map(|i| ((i / 300) % 7) as u8 * 30).collect(),
        ];
        for c in cases {
            assert_eq!(inflate_fixed(&zlib(&c)), c);
        }
        // a flat image compresses to almost nothing
        assert!(zlib(&vec![0; 1_000_000]).len() < 20_000);
    }

    /// Times the encoder on real screen content: `DVSNIP_BENCH=1 cargo test --release bench_ -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn bench_encode() {
        if std::env::var_os("DVSNIP_BENCH").is_none() {
            return;
        }
        let screen = crate::capture::virtual_screen();
        for r in [crate::image::Rect::new(300, 200, 1000, 650), screen] {
            let img = crate::capture::grab_rect(r).unwrap();
            let t = std::time::Instant::now();
            let png = encode(&img);
            println!(
                "{}x{}: {:.1} ms, {} KB (raw {} KB)",
                img.w,
                img.h,
                t.elapsed().as_secs_f64() * 1e3,
                png.len() / 1024,
                img.w * img.h * 3 / 1024
            );
        }
    }

    #[test]
    fn png_structure() {
        let img = Image {
            w: 3,
            h: 2,
            px: vec![0xFF11_2233, 0xFF44_5566, 0xFF77_8899, 0xFF00_0000, 0xFFFF_FFFF, 0xFF12_3456],
        };
        let p = encode(&img);
        assert_eq!(&p[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(&p[12..16], b"IHDR");
        assert_eq!(p[25], 2); // RGB
        let idat_len = u32::from_be_bytes(p[33..37].try_into().unwrap()) as usize;
        let raw = inflate_fixed(&p[41..41 + idat_len]);
        assert_eq!(raw.len(), 2 * (1 + 9));
        // undo the filters and compare with the pixels
        let mut rows: Vec<Vec<u8>> = Vec::new();
        for r in raw.chunks(10) {
            let f = r[0];
            let mut row = r[1..].to_vec();
            for i in 0..9 {
                let left = if i >= 3 { row[i - 3] } else { 0 };
                let up = rows.last().map_or(0, |p| p[i]);
                row[i] = row[i].wrapping_add(match f {
                    0 => 0,
                    1 => left,
                    2 => up,
                    _ => panic!(),
                });
            }
            rows.push(row);
        }
        assert_eq!(rows[0], vec![0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99]);
        assert_eq!(rows[1], vec![0, 0, 0, 0xFF, 0xFF, 0xFF, 0x12, 0x34, 0x56]);
        assert!(p.ends_with(&[0, 0, 0, 0, b'I', b'E', b'N', b'D', 0xAE, 0x42, 0x60, 0x82]));
        // transparency makes it RGBA
        let t = Image { w: 1, h: 1, px: vec![0] };
        assert_eq!(encode(&t)[25], 6);
    }
}
