#!/usr/bin/env sh
# Build a standalone, double-clickable macOS app bundle (+ a portable zip) from the
# release binary. No third-party tooling — just the system sips / iconutil /
# codesign / ditto. macOS only (the platform FFI is macOS).
#
#   sh tools/bundle-macos.sh
#
# Produces:
#   dist/aiTerminal.app   — drag to /Applications, or double-click to run
#   dist/aiTerminal.zip   — portable artifact to hand out
set -eu

case "$(uname -s)" in
    Darwin) ;;
    *) echo "bundle-macos.sh: macOS only (this host is $(uname -s))" >&2; exit 1 ;;
esac

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

APP_NAME="aiTerminal"
BIN="aiTerminal"
DIST="dist"
APP="$DIST/$APP_NAME.app"
CONTENTS="$APP/Contents"

echo "==> building release binary"
cargo build --release --bin "$BIN"

echo "==> rendering app icon"
mkdir -p "$DIST"
"target/release/$BIN" --render-icon "$DIST/icon.png"

echo "==> building AppIcon.icns"
ICONSET="$DIST/AppIcon.iconset"
rm -rf "$ICONSET"
mkdir -p "$ICONSET"
for s in 16 32 128 256 512; do
    sips -z "$s" "$s" "$DIST/icon.png" --out "$ICONSET/icon_${s}x${s}.png" >/dev/null
    d=$((s * 2))
    sips -z "$d" "$d" "$DIST/icon.png" --out "$ICONSET/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$DIST/AppIcon.icns"

echo "==> assembling $APP"
rm -rf "$APP"
mkdir -p "$CONTENTS/MacOS" "$CONTENTS/Resources"
cp "target/release/$BIN" "$CONTENTS/MacOS/$BIN"
cp "$DIST/AppIcon.icns" "$CONTENTS/Resources/AppIcon.icns"
cp "packaging/Info.plist" "$CONTENTS/Info.plist"
printf 'APPL????' > "$CONTENTS/PkgInfo"

# Bundle the read-only builtin registry (plugins/themes/keymaps/AI data) so it works in the
# distributed app (the binary resolves Contents/Resources/builtin at runtime).
echo "==> bundling the builtin registry"
cp -R "builtin" "$CONTENTS/Resources/builtin"

echo "==> ad-hoc code-signing (lets it run locally)"
codesign --force --deep --sign - "$APP"

echo "==> zipping portable artifact"
rm -f "$DIST/aiTerminal.zip"
ditto -c -k --keepParent "$APP" "$DIST/aiTerminal.zip"

# Tidy up intermediates.
rm -rf "$ICONSET" "$DIST/icon.png" "$DIST/AppIcon.icns"

echo ""
echo "Done."
echo "  app: $APP"
echo "  zip: $DIST/aiTerminal.zip"
echo ""
echo "Run it:      open \"$APP\""
echo "Install it:  cp -R \"$APP\" /Applications/   (then launch from Spotlight/Dock)"
