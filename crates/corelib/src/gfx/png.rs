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
mod tests {
    use super::*;
    use crate::gfx::Canvas;

    // ── A reference INFLATE, test-only ───────────────────────────────────────
    // Decoding is the only honest way to prove the encoder: every test below
    // round-trips through this. It handles stored + fixed blocks — exactly what
    // `zlib` emits — and nothing else.

    struct BitReader<'a> {
        d: &'a [u8],
        pos: usize,
        acc: u32,
        n: u32,
    }

    impl<'a> BitReader<'a> {
        fn bits(&mut self, n: u32) -> u32 {
            while self.n < n {
                let byte = *self.d.get(self.pos).expect("truncated deflate stream");
                self.pos += 1;
                self.acc |= (byte as u32) << self.n;
                self.n += 8;
            }
            let v = self.acc & ((1u32 << n) - 1);
            self.acc >>= n;
            self.n -= n;
            v
        }
        /// A Huffman code arrives most-significant bit first.
        fn code(&mut self, n: u32) -> u32 {
            let mut v = 0;
            for _ in 0..n {
                v = (v << 1) | self.bits(1);
            }
            v
        }
        fn fixed_symbol(&mut self) -> u16 {
            let v = self.code(7);
            if v <= 0x17 {
                return 256 + v as u16;
            }
            let v = (v << 1) | self.bits(1);
            if (0x30..=0xBF).contains(&v) {
                return (v - 0x30) as u16;
            }
            if (0xC0..=0xC7).contains(&v) {
                return 280 + (v - 0xC0) as u16;
            }
            let v = (v << 1) | self.bits(1);
            144 + (v - 0x190) as u16
        }
    }

    fn inflate(z: &[u8]) -> Vec<u8> {
        assert_eq!(z[0] & 0x0f, 8, "zlib CM must be deflate");
        let mut r = BitReader { d: z, pos: 2, acc: 0, n: 0 };
        let mut out = Vec::new();
        loop {
            let last = r.bits(1);
            let kind = r.bits(2);
            match kind {
                0 => {
                    r.acc = 0;
                    r.n = 0; // stored blocks are byte-aligned
                    let len = u16::from_le_bytes([r.d[r.pos], r.d[r.pos + 1]]) as usize;
                    r.pos += 4; // LEN + NLEN
                    out.extend_from_slice(&r.d[r.pos..r.pos + len]);
                    r.pos += len;
                }
                1 => loop {
                    let sym = r.fixed_symbol();
                    if sym == 256 {
                        break;
                    }
                    if sym < 256 {
                        out.push(sym as u8);
                        continue;
                    }
                    let k = (sym - 257) as usize;
                    let len = LEN_BASE[k] as usize + r.bits(LEN_EXTRA[k]) as usize;
                    let dc = r.code(5) as usize;
                    let dist = DIST_BASE[dc] as usize + r.bits(DIST_EXTRA[dc]) as usize;
                    let start = out.len() - dist;
                    for m in 0..len {
                        out.push(out[start + m]); // may overlap — that's the point
                    }
                },
                _ => panic!("unexpected BTYPE {kind}"),
            }
            if last == 1 {
                break;
            }
        }
        let want = u32::from_be_bytes([z[z.len() - 4], z[z.len() - 3], z[z.len() - 2], z[z.len() - 1]]);
        assert_eq!(adler32(&out), want, "adler-32 mismatch");
        out
    }

    /// Undo `filter_scanlines`, so a test can compare against the original pixels.
    fn unfilter(data: &[u8], width: u32, height: u32) -> Vec<u8> {
        let row = width as usize * BPP;
        let mut out: Vec<u8> = Vec::with_capacity(row * height as usize);
        for y in 0..height as usize {
            let kind = data[y * (row + 1)];
            let src = &data[y * (row + 1) + 1..y * (row + 1) + 1 + row];
            for i in 0..row {
                let a = if i >= BPP { out[y * row + i - BPP] } else { 0 };
                let b = if y > 0 { out[(y - 1) * row + i] } else { 0 };
                let c = if y > 0 && i >= BPP { out[(y - 1) * row + i - BPP] } else { 0 };
                let v = match kind {
                    0 => src[i],
                    1 => src[i].wrapping_add(a),
                    2 => src[i].wrapping_add(b),
                    3 => src[i].wrapping_add(((a as u16 + b as u16) / 2) as u8),
                    _ => src[i].wrapping_add(paeth(a, b, c)),
                };
                out.push(v);
            }
        }
        out
    }

    /// Pull the (single) IDAT payload back out of an encoded PNG.
    fn idat(png: &[u8]) -> Vec<u8> {
        let mut i = 8;
        let mut acc = Vec::new();
        while i + 8 <= png.len() {
            let len = u32::from_be_bytes([png[i], png[i + 1], png[i + 2], png[i + 3]]) as usize;
            let kind = &png[i + 4..i + 8];
            if kind == b"IDAT" {
                acc.extend_from_slice(&png[i + 8..i + 8 + len]);
            }
            i += 12 + len;
        }
        acc
    }

    // A cheap deterministic PRNG — incompressible input is the encoder's worst case.
    fn pseudo_random(n: usize) -> Vec<u8> {
        let mut s = 0x2545_F491_4F6C_DD1Du64;
        (0..n)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                (s >> 33) as u8
            })
            .collect()
    }

    #[test]
    fn deflate_round_trips_every_input_shape() {
        for raw in [
            Vec::new(),
            b"a".to_vec(),
            b"hello hello hello hello".to_vec(),
            vec![0u8; 200_000],                    // one enormous run — the flat-image case
            pseudo_random(50_000),                 // incompressible
            [vec![7u8; 40_000], pseudo_random(9_000)].concat(),
        ] {
            assert_eq!(inflate(&zlib(&raw)), raw, "round-trip failed for {} bytes", raw.len());
        }
    }

    #[test]
    fn deflate_matches_across_the_full_window() {
        // A repeat at ~32 KiB exercises the longest distance codes; the tail forces
        // a match whose distance is near the window limit.
        let mut raw = pseudo_random(WINDOW - 100);
        raw.extend_from_slice(&raw[0..300].to_vec());
        assert_eq!(inflate(&zlib(&raw)), raw);
    }

    #[test]
    fn flat_images_compress_by_more_than_ten_times() {
        // The property that makes `/shot` sendable: a terminal frame is mostly one
        // background color, and must not cost `w*h*4` bytes on the wire.
        let (w, h) = (400u32, 300u32);
        let px = vec![0x20u8; (w * h * 4) as usize];
        let png = encode_rgba8(&px, w, h);
        assert!(png.len() * 10 < px.len(), "expected >10x, got {} -> {}", px.len(), png.len());
    }

    #[test]
    fn encoded_pixels_survive_filtering_and_compression() {
        let (w, h) = (23u32, 9u32); // deliberately not a round number of anything
        let px = pseudo_random((w * h * 4) as usize);
        let png = encode_rgba8(&px, w, h);
        assert_eq!(unfilter(&inflate(&idat(&png)), w, h), px);
    }

    #[test]
    fn a_rendered_surface_round_trips() {
        let mut s = Surface::new(64, 40);
        s.clear(crate::types::Rgba8::new(18, 18, 24, 255));
        s.fill_rect(crate::types::Rect::new(8.0, 8.0, 30.0, 12.0), crate::types::Rgba8::new(220, 80, 60, 255));
        let png = encode_surface(&s);
        let pixels = unfilter(&inflate(&idat(&png)), 64, 40);
        assert_eq!(pixels.len(), 64 * 40 * 4);
        // top-left is the cleared background, straight (un-premultiplied) RGBA
        assert_eq!(&pixels[0..4], &[18, 18, 24, 255]);
        // inside the rect
        let off = ((12 * 64) + 12) * 4;
        assert_eq!(&pixels[off..off + 4], &[220, 80, 60, 255]);
    }

    #[test]
    fn emits_a_well_formed_png() {
        // 2x2 RGBA: red, green, blue, yellow.
        let px = [255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 0, 255];
        let png = encode_rgba8(&px, 2, 2);
        assert_eq!(&png[..8], &[137, 80, 78, 71, 13, 10, 26, 10], "signature");
        assert_eq!(&png[12..16], b"IHDR");
        assert_eq!(u32::from_be_bytes([png[16], png[17], png[18], png[19]]), 2, "width");
        assert_eq!(u32::from_be_bytes([png[20], png[21], png[22], png[23]]), 2, "height");
        assert_eq!(png[24], 8, "bit depth");
        assert_eq!(png[25], 6, "color type RGBA");
        assert_eq!(&png[png.len() - 8..png.len() - 4], b"IEND");
        assert_eq!(unfilter(&inflate(&idat(&png)), 2, 2), px);
    }

    #[test]
    fn surface_alpha_round_trips_straight() {
        let mut s = Surface::new(1, 1);
        // half-transparent orange, premultiplied internally
        s.clear(crate::types::Rgba8::new(200, 100, 50, 128));
        let png = encode_surface(&s);
        assert_eq!(u32::from_be_bytes([png[16], png[17], png[18], png[19]]), 1);
        assert_eq!(png[25], 6);
        let pixels = unfilter(&inflate(&idat(&png)), 1, 1);
        // straight alpha must survive the un-premultiply (±1 for the rounding trip).
        assert_eq!(pixels[3], 128, "alpha");
        assert!((pixels[0] as i16 - 200).abs() <= 2, "red {}", pixels[0]);
    }
}
