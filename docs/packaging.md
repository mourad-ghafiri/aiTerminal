# Packaging (macOS)

`tools/bundle-macos.sh` builds a self-contained `aiTerminal.app`:

1. `cargo build --release --target <triple>` — one binary, zero external crates.
2. Renders the app icon headlessly (`aiTerminal --render-icon`) and converts it via
   `sips`/`iconutil`. Once — the `.icns` is architecture-independent.
3. Copies the binary + `builtin/` bundle into
   `aiTerminal.app/Contents/{MacOS,Resources}` with the Info.plist, then ad-hoc
   signs it (`codesign --sign -`) and zips it with `ditto`.

The runtime resolves the bundled `builtin/` next to the binary
(`Contents/Resources/builtin`), so the .app is drag-and-drop installable. `TT_BIN`
is exported into shells so `@`-commands find the binary even though the bundle isn't
on PATH.

## Architectures

macOS runs on two CPU architectures and a build for one does **not** run on the
other (Rosetta aside). The script takes them as arguments:

| Command | Builds |
| --- | --- |
| `./tools/bundle-macos.sh` | this Mac's architecture (the dev loop) |
| `./tools/bundle-macos.sh arm64` | Apple Silicon (`aarch64-apple-darwin`) |
| `./tools/bundle-macos.sh x86_64` | Intel (`x86_64-apple-darwin`) |
| `./tools/bundle-macos.sh universal` | one binary with both slices, via `lipo` |
| `./tools/bundle-macos.sh all` | all three — the release set |

Output per architecture `<arch>`:

```text
dist/<arch>/aiTerminal.app          the bundle (always named aiTerminal.app)
dist/aiTerminal-macos-<arch>.zip    the release artifact
dist/aiTerminal.app                 a copy of whichever build runs on THIS Mac
```

Cross-building needs the other standard library, installed once:

```sh
rustup target add aarch64-apple-darwin x86_64-apple-darwin
```

The script checks for it and tells you the exact command instead of failing deep
inside cargo. Ad-hoc signatures are per-slice, so a universal bundle is `lipo`'d
first and signed after — never the reverse.

The website links the three zips by name
(`releases/latest/download/aiTerminal-macos-<arch>.zip`), so a release must
publish them under exactly those names.

## Why both architectures matter beyond packaging

The macOS backend is raw Objective-C runtime FFI, and the two architectures do
not share a calling convention: on x86_64 a struct larger than 16 bytes
(`CGRect`) is returned through a hidden pointer, which needs libobjc's separate
`objc_msgSend_stret` entry point — sending it through plain `objc_msgSend`
segfaults on Intel while being perfectly fine on Apple Silicon. `msg_send!`
picks the entry point from the return type (`crates/platform/src/os/macos/objc.rs`),
and the shim's ABI tests run on whichever architecture you test — so run them on
both before a release:

```sh
cargo test -p platform --lib objc                              # host
cargo test -p platform --lib --target x86_64-apple-darwin objc # the other one
```
