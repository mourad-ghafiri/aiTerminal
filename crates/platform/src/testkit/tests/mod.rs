use super::*;

#[test]
fn scripted_pty_reads_script_then_eof() {
    let p = ScriptedPty::new(b"hello");
    let mut buf = [0u8; 3];
    assert_eq!(p.read(&mut buf).unwrap(), 3);
    assert_eq!(&buf, b"hel");
    assert_eq!(p.read(&mut buf).unwrap(), 2);
    assert_eq!(&buf[..2], b"lo");
    assert_eq!(p.read(&mut buf).unwrap(), 0); // EOF
}

#[test]
fn scripted_pty_captures_writes_and_resize() {
    let p = ScriptedPty::new(b"");
    p.write(b"ls\n").unwrap();
    p.resize(120, 40).unwrap();
    assert_eq!(p.written(), b"ls\n");
    assert_eq!(p.last_size(), (120, 40));
}

#[test]
fn mock_gpu_captures_frame() {
    let mut g = MockGpu::new();
    g.present(&[1, 2, 3, 4], 2, 2, None);
    assert_eq!(g.present_count, 1);
    assert_eq!(g.frame().0, vec![1, 2, 3, 4]);
}

#[test]
fn mock_shaper_blank_space_solid_letter() {
    let s = MockShaper;
    assert!(s.rasterize(' ', 20.0).unwrap().is_blank());
    let g = s.rasterize('A', 20.0).unwrap();
    assert!(!g.is_blank());
    assert!(g.coverage.iter().all(|&c| c == 255));
}
