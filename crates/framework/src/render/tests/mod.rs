use super::*;

#[test]
fn write_ppm_emits_a_valid_p6_frame() {
    let path = std::env::temp_dir().join(format!("tt-ppm-{}.ppm", std::process::id()));
    // 2×1: a red and a blue pixel (0xRRGGBB in the u32 layout the renderer uses).
    write_ppm(&path.to_string_lossy(), &[0x00FF0000, 0x000000FF], 2, 1).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    assert!(bytes.starts_with(b"P6\n2 1\n255\n"), "PPM header");
    assert_eq!(&bytes[bytes.len() - 6..], &[0xFF, 0, 0, 0, 0, 0xFF], "RGB triplets in order");
    let _ = std::fs::remove_file(&path);
}
