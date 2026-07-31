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
