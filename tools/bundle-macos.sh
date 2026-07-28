#!/usr/bin/env sh
# Build standalone, double-clickable macOS app bundles (+ portable zips) from the
# release binary — for either CPU architecture, or both. No third-party tooling —
# just the system sips / iconutil / lipo / codesign / ditto. macOS only (the
# platform FFI is macOS).
#
#   sh tools/bundle-macos.sh                # this Mac's architecture (fast, the dev loop)
#   sh tools/bundle-macos.sh arm64          # Apple Silicon
#   sh tools/bundle-macos.sh x86_64         # Intel
#   sh tools/bundle-macos.sh universal      # one binary, both slices (lipo)
#   sh tools/bundle-macos.sh all            # arm64 + x86_64 + universal (release set)
#
# Cross-building needs the other standard library, once:
#   rustup target add aarch64-apple-darwin x86_64-apple-darwin
#
# Produces, per architecture <arch>:
#   dist/<arch>/aiTerminal.app            — drag to /Applications
#   dist/aiTerminal-macos-<arch>.zip      — the release artifact to hand out
# plus dist/aiTerminal.app — a copy of whichever build runs on THIS Mac, so
# `open dist/aiTerminal.app` always works.
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

case "$(uname -m)" in
    arm64) HOST_ARCH="arm64" ;;
    x86_64) HOST_ARCH="x86_64" ;;
    *) echo "bundle-macos.sh: unsupported host CPU $(uname -m)" >&2; exit 1 ;;
esac

# arch label -> rust target triple
triple_for() {
    case "$1" in
        arm64) echo "aarch64-apple-darwin" ;;
        x86_64) echo "x86_64-apple-darwin" ;;
        *) echo "bundle-macos.sh: no triple for '$1'" >&2; exit 1 ;;
    esac
}

# Which labels were requested (deduped, in a stable order).
ARCHS=""
want() {
    case " $ARCHS " in *" $1 "*) ;; *) ARCHS="${ARCHS:+$ARCHS }$1" ;; esac
}
for arg in "$@"; do
    case "$arg" in
        arm64 | aarch64 | apple-silicon) want arm64 ;;
        x86_64 | x86-64 | x64 | intel) want x86_64 ;;
        universal | fat) want universal ;;
        all) want arm64; want x86_64; want universal ;;
        host | native) want "$HOST_ARCH" ;;
        -h | --help)
            sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            echo "bundle-macos.sh: unknown argument '$arg' (try --help)" >&2
            exit 2
            ;;
    esac
done
[ -n "$ARCHS" ] || ARCHS="$HOST_ARCH"

# Every requested label expanded to the slices that must actually be compiled.
SLICES=""
for a in $ARCHS; do
    case "$a" in
        universal)
            case " $SLICES " in *" arm64 "*) ;; *) SLICES="${SLICES:+$SLICES }arm64" ;; esac
            case " $SLICES " in *" x86_64 "*) ;; *) SLICES="${SLICES:+$SLICES }x86_64" ;; esac
            ;;
        *) case " $SLICES " in *" $a "*) ;; *) SLICES="${SLICES:+$SLICES }$a" ;; esac ;;
    esac
done

# Fail early and actionably on a missing cross std, rather than deep inside cargo.
for s in $SLICES; do
    t="$(triple_for "$s")"
    if command -v rustup >/dev/null 2>&1 && ! rustup target list --installed | grep -qx "$t"; then
        echo "bundle-macos.sh: the $s standard library is not installed." >&2
        echo "  run: rustup target add $t" >&2
        exit 1
    fi
done

mkdir -p "$DIST"

echo "==> building release binaries: $SLICES"
for s in $SLICES; do
    t="$(triple_for "$s")"
    echo "--> $s ($t)"
    cargo build --release --target "$t" --bin "$BIN"
done

slice_bin() { echo "target/$(triple_for "$1")/release/$BIN"; }

# The icon is architecture-independent, so render it once — with a binary this Mac
# can actually execute (the host slice; failing that, a native build just for this).
echo "==> rendering app icon"
ICON_BIN=""
for s in $SLICES; do
    [ "$s" = "$HOST_ARCH" ] && ICON_BIN="$(slice_bin "$s")"
done
if [ -z "$ICON_BIN" ]; then
    echo "--> cross-build only; building a host binary to render the icon"
    cargo build --release --target "$(triple_for "$HOST_ARCH")" --bin "$BIN"
    ICON_BIN="$(slice_bin "$HOST_ARCH")"
fi
"$ICON_BIN" --render-icon "$DIST/icon.png"

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

# Assemble one .app for an arch label. The bundle is always named aiTerminal.app
# (it is what lands in /Applications); the architecture lives in the staging
# directory and the zip name.
bundle() {
    arch="$1"
    app="$DIST/$arch/$APP_NAME.app"
    contents="$app/Contents"

    echo "==> assembling $app"
    rm -rf "$DIST/$arch"
    mkdir -p "$contents/MacOS" "$contents/Resources"

    if [ "$arch" = "universal" ]; then
        lipo -create "$(slice_bin arm64)" "$(slice_bin x86_64)" -output "$contents/MacOS/$BIN"
    else
        cp "$(slice_bin "$arch")" "$contents/MacOS/$BIN"
    fi

    cp "$DIST/AppIcon.icns" "$contents/Resources/AppIcon.icns"
    cp "packaging/Info.plist" "$contents/Info.plist"
    printf 'APPL????' > "$contents/PkgInfo"

    # Bundle the read-only builtin registry (plugins/themes/keymaps/AI data) so it works
    # in the distributed app (the binary resolves Contents/Resources/builtin at runtime).
    cp -R "builtin" "$contents/Resources/builtin"

    # Ad-hoc code-signing (lets it run locally). Signing is per-slice, so this must
    # happen after lipo, never before.
    codesign --force --deep --sign - "$app"

    zip="$DIST/$APP_NAME-macos-$arch.zip"
    rm -f "$zip"
    ditto -c -k --keepParent "$app" "$zip"
    echo "    $zip"
}

for a in $ARCHS; do
    bundle "$a"
done

# A copy that runs on THIS Mac, at the documented path.
LOCAL=""
for a in $ARCHS; do
    [ "$a" = "$HOST_ARCH" ] && LOCAL="$a"
done
if [ -z "$LOCAL" ]; then
    for a in $ARCHS; do
        [ "$a" = "universal" ] && LOCAL="universal"
    done
fi
if [ -n "$LOCAL" ]; then
    rm -rf "$DIST/$APP_NAME.app"
    cp -R "$DIST/$LOCAL/$APP_NAME.app" "$DIST/$APP_NAME.app"
fi

# Tidy up intermediates.
rm -rf "$ICONSET" "$DIST/icon.png" "$DIST/AppIcon.icns"

echo ""
echo "Done."
for a in $ARCHS; do
    echo "  $a: $DIST/$a/$APP_NAME.app  ·  $DIST/$APP_NAME-macos-$a.zip"
done
if [ -n "$LOCAL" ]; then
    echo ""
    echo "Run it:      open \"$DIST/$APP_NAME.app\"            ($LOCAL — runs on this Mac)"
    echo "Install it:  cp -R \"$DIST/$APP_NAME.app\" /Applications/   (then launch from Spotlight/Dock)"
else
    echo ""
    echo "Note: nothing built here runs on this $HOST_ARCH Mac."
fi
