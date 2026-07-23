# Packaging (macOS)

`tools/bundle-macos.sh` builds a self-contained `aiTerminal.app`:

1. `cargo build --release` — one binary, zero external crates.
2. Renders the app icon headlessly (`aiTerminal --render-icon`) and converts it via
   `sips`/`iconutil`.
3. Copies the binary + `builtin/` bundle into
   `aiTerminal.app/Contents/{MacOS,Resources}` with the Info.plist.

The runtime resolves the bundled `builtin/` next to the binary
(`Contents/Resources/builtin`), so the .app is drag-and-drop installable. `TT_BIN`
is exported into shells so `@`-commands find the binary even though the bundle isn't
on PATH.
