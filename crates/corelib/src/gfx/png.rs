//! A minimal, std-only PNG encoder (no third-party crate, no FFI).
//!
//! Truecolor + alpha (8-bit RGBA) with per-scanline filter selection and a
//! fixed-Huffman DEFLATE stream (LZ77 over a 32 KiB window). Screen captures are
//! mostly flat color, so this is the difference between a multi-megabyte frame and
//! a few hundred kilobytes — which is what makes sending one over a chat gateway
//! (`@gate`) practical at all.
//!
//! Fixed Huffman (BTYPE=01) rather than dynamic: no code-length tables to build or
//! get wrong, and on this kind of image the LZ77 matches — not the symbol coding —
//! are where the compression comes from.

use crate::gfx::Surface;

/// Encode straight (non-premultiplied) RGBA8 pixels (`width*height*4` bytes,
/// row-major, top-left origin) as a PNG byte stream.
pub fn encode_rgba8(pixels: &[u8], width: u32, height: u32) -> Vec<u8> {
    assert_eq!(pixels.len(), width as usize * height as usize * 4, "pixel buffer size mismatch");
    let mut out = Vec::with_capacity(pixels.len() / 8 + 1024);
    out.extend_from_slice(&[137, 80, 78, 71, 13, 10, 26, 10]); // PNG signature

    // IHDR: width, height, bit depth 8, color type 6 (RGBA), default comp/filter/interlace.
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);

    chunk(&mut out, b"IDAT", &zlib(&filter_scanlines(pixels, width, height)));

    chunk(&mut out, b"IEND", &[]);
    out
}

/// Encode a [`Surface`] (premultiplied-BGRA8) as PNG, un-premultiplying to straight
/// RGBA so transparent / anti-aliased-edge pixels round-trip correctly.
pub fn encode_surface(surf: &Surface) -> Vec<u8> {
    let px = surf.pixels();
    let mut rgba = Vec::with_capacity(px.len() * 4);
    for &p in px {
        let a = (p >> 24) & 0xff;
        let (r, g, b) = ((p >> 16) & 0xff, (p >> 8) & 0xff, p & 0xff);
        let (sr, sg, sb) = match a {
            0 => (0, 0, 0),
            255 => (r, g, b),
            _ => (unpremul(r, a), unpremul(g, a), unpremul(b, a)),
        };
        rgba.extend_from_slice(&[sr as u8, sg as u8, sb as u8, a as u8]);
    }
    encode_rgba8(&rgba, surf.width(), surf.height())
}

/// Encode a [`Surface`] to a PNG file.
pub fn write_surface(path: &str, surf: &Surface) -> std::io::Result<()> {
    std::fs::write(path, encode_surface(surf))
}

#[inline]
fn unpremul(c: u32, a: u32) -> u32 {
    ((c * 255 + a / 2) / a).min(255)
}

/// Write a PNG chunk: `len(BE) | type | data | crc32(type+data, BE)`.
fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut crc = !0u32;
    for &b in kind.iter().chain(data) {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    out.extend_from_slice(&(crc ^ !0u32).to_be_bytes());
}

// ── PNG scanline filtering ───────────────────────────────────────────────────

/// Bytes per pixel for RGBA8 — the filter "left neighbour" distance.
const BPP: usize = 4;

/// Apply the best of the five PNG filters to each scanline, prefixing the chosen
/// filter type. Filtering is what turns a run of identical pixels (most of a
/// terminal frame) into a run of zero bytes for DEFLATE to collapse.
fn filter_scanlines(pixels: &[u8], width: u32, height: u32) -> Vec<u8> {
    let row = width as usize * BPP;
    let mut out = Vec::with_capacity(height as usize * (row + 1));
    let mut cand = vec![0u8; row * 5];
    let zero = vec![0u8; row];
    for y in 0..height as usize {
        let cur = &pixels[y * row..y * row + row];
        let prior = if y == 0 { &zero[..] } else { &pixels[(y - 1) * row..(y - 1) * row + row] };
        let (mut best, mut best_cost) = (0usize, u64::MAX);
        for f in 0..5usize {
            let dst = &mut cand[f * row..f * row + row];
            filter_row(f as u8, cur, prior, dst);
            // Minimum sum of absolute (signed) deviations — the standard heuristic:
            // the filter whose output is closest to zero compresses best.
            let cost: u64 = dst.iter().map(|&b| (b as i8).unsigned_abs() as u64).sum();
            if cost < best_cost {
                best_cost = cost;
                best = f;
            }
        }
        out.push(best as u8);
        out.extend_from_slice(&cand[best * row..best * row + row]);
    }
    out
}

fn filter_row(kind: u8, cur: &[u8], prior: &[u8], dst: &mut [u8]) {
    for i in 0..cur.len() {
        let a = if i >= BPP { cur[i - BPP] } else { 0 }; // left
        let b = prior[i]; // above
        let c = if i >= BPP { prior[i - BPP] } else { 0 }; // upper-left
        dst[i] = match kind {
            0 => cur[i],
            1 => cur[i].wrapping_sub(a),
            2 => cur[i].wrapping_sub(b),
            3 => cur[i].wrapping_sub(((a as u16 + b as u16) / 2) as u8),
            _ => cur[i].wrapping_sub(paeth(a, b, c)),
        };
    }
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = a as i16 + b as i16 - c as i16;
    let (pa, pb, pc) = ((p - a as i16).abs(), (p - b as i16).abs(), (p - c as i16).abs());
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

// ── DEFLATE (fixed Huffman + LZ77) ───────────────────────────────────────────

const MIN_MATCH: usize = 3;
const MAX_MATCH: usize = 258;
const WINDOW: usize = 32768;
const HASH_BITS: u32 = 15;
const HASH_SIZE: usize = 1 << HASH_BITS;
/// How far down a hash chain to look. 32 is the usual speed/ratio knee — screen
/// captures match on the first few candidates anyway.
const MAX_CHAIN: usize = 32;
const NIL: u32 = u32::MAX;

/// Length codes 257..=285: the first length each covers, and its extra-bit count.
const LEN_BASE: [u16; 29] =
    [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258];
const LEN_EXTRA: [u32; 29] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
/// Distance codes 0..=29.
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145,
    8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u32; 30] =
    [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];

/// DEFLATE bit packing: bytes fill from the least-significant bit, but a Huffman
/// code goes in most-significant-bit first (RFC 1951 §3.1.1).
struct BitWriter {
    out: Vec<u8>,
    acc: u32,
    n: u32,
}

impl BitWriter {
    fn new(cap: usize) -> Self {
        BitWriter { out: Vec::with_capacity(cap), acc: 0, n: 0 }
    }
    /// Write `n` bits of `val`, least-significant bit first (extra bits, headers).
    fn bits(&mut self, val: u32, n: u32) {
        self.acc |= val << self.n;
        self.n += n;
        while self.n >= 8 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
    }
    /// Write a Huffman code of `n` bits, most-significant bit first.
    fn code(&mut self, val: u32, n: u32) {
        for i in (0..n).rev() {
            self.bits((val >> i) & 1, 1);
        }
    }
    fn finish(mut self) -> Vec<u8> {
        if self.n > 0 {
            self.out.push(self.acc as u8);
        }
        self.out
    }
}

/// The fixed literal/length alphabet (RFC 1951 §3.2.6) as `(code, bit length)`.
fn fixed_lit(sym: u16) -> (u32, u32) {
    match sym {
        0..=143 => (0x30 + sym as u32, 8),
        144..=255 => (0x190 + (sym as u32 - 144), 9),
        256..=279 => (sym as u32 - 256, 7),
        _ => (0xC0 + (sym as u32 - 280), 8),
    }
}

#[inline]
fn hash3(d: &[u8], i: usize) -> usize {
    let v = (d[i] as u32) << 16 | (d[i + 1] as u32) << 8 | d[i + 2] as u32;
    (v.wrapping_mul(0x9E37_79B1) >> (32 - HASH_BITS)) as usize
}

/// A zlib stream (RFC 1950) wrapping `raw` in one fixed-Huffman DEFLATE block.
fn zlib(raw: &[u8]) -> Vec<u8> {
    let mut w = BitWriter::new(raw.len() / 4 + 64);
    w.out.extend_from_slice(&[0x78, 0x01]); // CMF = deflate/32K window, FLG = check bits ok
    w.bits(1, 1); // BFINAL — one block for the whole image
    w.bits(1, 2); // BTYPE = 01, fixed Huffman

    let n = raw.len();
    let mut head = vec![NIL; HASH_SIZE];
    let mut prev = vec![NIL; n];
    let mut i = 0usize;
    while i < n {
        let (mut best_len, mut best_dist) = (0usize, 0usize);
        if i + MIN_MATCH <= n {
            let limit = (n - i).min(MAX_MATCH);
            let mut cand = head[hash3(raw, i)];
            let mut chain = MAX_CHAIN;
            while cand != NIL && chain > 0 {
                let j = cand as usize;
                let dist = i - j;
                if dist > WINDOW {
                    break;
                }
                let mut l = 0;
                while l < limit && raw[j + l] == raw[i + l] {
                    l += 1;
                }
                if l > best_len {
                    best_len = l;
                    best_dist = dist;
                    if l == limit {
                        break; // can't do better here
                    }
                }
                cand = prev[j];
                chain -= 1;
            }
        }

        let advance = if best_len >= MIN_MATCH {
            let (lc, extra, eb) = length_code(best_len);
            let (code, bits) = fixed_lit(lc);
            w.code(code, bits);
            if eb > 0 {
                w.bits(extra, eb);
            }
            let (dc, dextra, deb) = distance_code(best_dist);
            w.code(dc as u32, 5); // fixed distance codes are 5 plain bits, MSB first
            if deb > 0 {
                w.bits(dextra, deb);
            }
            best_len
        } else {
            let (code, bits) = fixed_lit(raw[i] as u16);
            w.code(code, bits);
            1
        };

        // Index every position the emitted token covered, so later matches can
        // start anywhere inside it.
        for k in i..i + advance {
            if k + MIN_MATCH <= n {
                let h = hash3(raw, k);
                prev[k] = head[h];
                head[h] = k as u32;
            }
        }
        i += advance;
    }

    let (eob, eob_bits) = fixed_lit(256);
    w.code(eob, eob_bits);
    let mut z = w.finish();
    z.extend_from_slice(&adler32(raw).to_be_bytes());
    z
}

/// `(symbol, extra-bit value, extra-bit count)` for a match length of 3..=258.
fn length_code(len: usize) -> (u16, u32, u32) {
    let mut k = LEN_BASE.len() - 1;
    while LEN_BASE[k] as usize > len {
        k -= 1;
    }
    (257 + k as u16, (len - LEN_BASE[k] as usize) as u32, LEN_EXTRA[k])
}

/// `(symbol, extra-bit value, extra-bit count)` for a distance of 1..=32768.
fn distance_code(dist: usize) -> (u16, u32, u32) {
    let mut k = DIST_BASE.len() - 1;
    while DIST_BASE[k] as usize > dist {
        k -= 1;
    }
    (k as u16, (dist - DIST_BASE[k] as usize) as u32, DIST_EXTRA[k])
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &x in data {
        a = (a + x as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

#[cfg(test)]
mod tests;
