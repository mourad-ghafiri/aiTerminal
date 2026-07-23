//! Per-OS system-library linking. No third-party build deps — just emits
//! `cargo:rustc-link-*` directives. (libSystem, which provides the PTY/fork/exec
//! and read/write syscalls, is linked automatically.)

fn main() {
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    match os.as_str() {
        "macos" => {
            for fw in [
                "Foundation",
                "CoreFoundation",
                "CoreGraphics",
                "CoreText",
                "ImageIO",
                "AppKit",
                "Metal",
                "QuartzCore",
                // Media playback (AVPlayer) + its time/pixel types.
                "AVFoundation",
                "CoreMedia",
                "CoreVideo",
            ] {
                println!("cargo:rustc-link-lib=framework={fw}");
            }
        }
        _ => {
            // Windows / Linux linking is configured when those backends land.
        }
    }
}
