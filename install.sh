#!/usr/bin/env sh
# aiTerminal installer — install, update and remove, from one command.
#
#   curl -fsSL https://mourad-ghafiri.github.io/aiTerminal/install.sh | sh
#   curl -fsSL .../install.sh | sh -s -- update
#   curl -fsSL .../install.sh | sh -s -- remove
#
# It clones (or updates) the source at the **newest release tag**, makes sure Rust is
# present, builds the macOS app with `tools/bundle-macos.sh`, and installs it to
# /Applications — asking first when it has a terminal to ask on, and telling you exactly
# where things went either way.
#
# A release here IS a tag (nothing publishes binaries), so that is what "install
# aiTerminal" means. Set AITERMINAL_REF to build something else — an older tag, a branch,
# a commit.
#
# Nothing here needs `sudo`: if /Applications isn't writable the app is installed to
# ~/Applications instead, which Spotlight and the Dock treat the same.
#
# Commands
#   install   (default)  clone/update the source, build, install the app
#   update               same as install — it is the one command for both
#   remove               delete the installed app (and the build; `--purge` your settings)
#   help                 this text
#
# Options
#   --universal          build one binary with both CPU slices (needs both stdlibs)
#   --arm64 | --x86_64   build for one architecture instead of this Mac's
#   --no-install         build only, and print where the app is
#   --dir <path>         install into <path> instead of /Applications
#   --yes                never ask (already the default when there is no terminal)
#   --purge              with `remove`: also delete ~/.aiTerminal (your settings)
#
# Environment
#   AITERMINAL_SRC       where the source lives (default ~/.local/share/aiTerminal/src)
#   AITERMINAL_REPO      the git remote to clone (default the official GitHub repo)
#   AITERMINAL_REF       tag/branch/commit to build (default: the newest release tag)
#   AITERMINAL_INSTALL_URL  where this script lives (only for the hints it prints)
set -eu

APP_NAME="aiTerminal"
REPO="${AITERMINAL_REPO:-https://github.com/mourad-ghafiri/aiTerminal.git}"
# Where this script itself lives — the project's GitHub Pages site, which serves the repo
# root. Only used for the "how to update / remove" hints it prints at the end.
INSTALL_URL="${AITERMINAL_INSTALL_URL:-https://mourad-ghafiri.github.io/aiTerminal/install.sh}"
SRC="${AITERMINAL_SRC:-$HOME/.local/share/aiTerminal/src}"
# Empty means "whatever the newest release tag is" — resolved against the remote below,
# because the answer changes between runs and this script is usually piped from the web.
REF="${AITERMINAL_REF:-}"

CMD="install"
ARCH_ARG=""
INSTALL_DIR=""
DO_INSTALL=1
ASSUME_YES=0
PURGE=0

# ── output ───────────────────────────────────────────────────────────────────
# Color only when stdout is a terminal, so piping to a file or a log stays clean.
if [ -t 1 ]; then
    BOLD=$(printf '\033[1m'); DIM=$(printf '\033[2m'); GREEN=$(printf '\033[32m')
    YELLOW=$(printf '\033[33m'); RED=$(printf '\033[31m'); RESET=$(printf '\033[0m')
else
    BOLD=""; DIM=""; GREEN=""; YELLOW=""; RED=""; RESET=""
fi

say()  { printf '%s==>%s %s\n' "$GREEN" "$RESET" "$*"; }
note() { printf '%s    %s%s\n' "$DIM" "$*" "$RESET"; }
warn() { printf '%s!%s   %s\n' "$YELLOW" "$RESET" "$*" >&2; }
die()  { printf '%serror:%s %s\n' "$RED" "$RESET" "$*" >&2; exit 1; }

# The help text lives here rather than being read back out of the file: `$0` is `sh` when
# this script arrives through a pipe, and there would be nothing to read.
usage() {
    cat <<'USAGE'
aiTerminal installer — install, update and remove, from one command.

  curl -fsSL https://mourad-ghafiri.github.io/aiTerminal/install.sh | sh
  curl -fsSL .../install.sh | sh -s -- update
  curl -fsSL .../install.sh | sh -s -- remove

It clones (or updates) the source at the newest release tag, makes sure Rust is present,
builds the macOS app with tools/bundle-macos.sh, and installs it to /Applications —
asking first when it has a terminal to ask on, and telling you exactly where things went
either way. Nothing here needs sudo: if /Applications isn't writable the app goes to
~/Applications instead.

Commands
  install   (default)  clone/update the source, build, install the app
  update               same as install — it is the one command for both
  remove               delete the installed app (and the build; --purge your settings)
  help                 this text

Options
  --universal          build one binary with both CPU slices (needs both stdlibs)
  --arm64 | --x86_64   build for one architecture instead of this Mac's
  --no-install         build only, and print where the app is
  --dir <path>         install into <path> instead of /Applications
  --yes                never ask (already the default when there is no terminal)
  --purge              with `remove`: also delete ~/.aiTerminal (your settings)

Environment
  AITERMINAL_SRC       where the source lives (default ~/.local/share/aiTerminal/src)
  AITERMINAL_REPO      the git remote to clone (default the official GitHub repo)
  AITERMINAL_REF       tag/branch/commit to build (default: the newest release tag)
  AITERMINAL_INSTALL_URL  where this script lives (only for the hints it prints)

  AITERMINAL_REF=v0.4.0 installs that release; AITERMINAL_REF=main builds the tip.
USAGE
}

# Ask a yes/no question on the *terminal* — never on stdin, which is the script
# itself when this is run as `curl | sh`. With no terminal, take the default.
confirm() {
    prompt="$1"
    [ "$ASSUME_YES" -eq 1 ] && return 0
    # `-r /dev/tty` can be true while opening it still fails (no controlling terminal —
    # CI, a launchd job, a container), so the open itself is the test. Probe in a subshell
    # first: a failed redirection on `exec` prints from the shell, past our own 2>/dev/null.
    ( : </dev/tty ) 2>/dev/null || return 0
    exec 3<>/dev/tty 2>/dev/null || return 0
    printf '%s%s%s [Y/n] ' "$BOLD" "$prompt" "$RESET" >&3
    read -r reply <&3 || reply=""
    exec 3>&-
    case "$reply" in
        "" | y | Y | yes | YES | Yes) return 0 ;;
        *) return 1 ;;
    esac
}

have() { command -v "$1" >/dev/null 2>&1; }

# The newest release tag on the remote, or nothing when the repo has none.
#
# Asked of the REMOTE, not of a local clone: this script usually arrives through a pipe
# with no checkout to consult, and even with one the local tags are as old as the last
# fetch. Only plain `vN.N.N` tags count — a release candidate is not a release, and a
# tag that is not a version is not a thing to install.
#
# The version sort is done here rather than with `--sort=-v:refname` (git ≥ 2.18) or
# `sort -V` (not in BSD sort, so not on macOS): pad each field to a fixed width and an
# ordinary reverse sort puts v0.10.0 above v0.9.0, everywhere.
latest_tag() {
    git ls-remote --tags --refs "$REPO" 2>/dev/null \
        | sed 's#.*refs/tags/##' \
        | grep -E '^v?[0-9]+(\.[0-9]+)*$' \
        | awk '{ v = $0; sub(/^v/, "", v); split(v, p, ".")
                 printf "%010d%010d%010d\t%s\n", p[1], p[2], p[3], $0 }' \
        | sort -r \
        | head -n1 \
        | cut -f2
}

# ── arguments ────────────────────────────────────────────────────────────────
while [ $# -gt 0 ]; do
    case "$1" in
        install | update | upgrade) CMD="install" ;;
        remove | uninstall) CMD="remove" ;;
        help | --help | -h) usage; exit 0 ;;
        --universal) ARCH_ARG="universal" ;;
        --arm64) ARCH_ARG="arm64" ;;
        --x86_64 | --intel) ARCH_ARG="x86_64" ;;
        --no-install) DO_INSTALL=0 ;;
        --yes | -y) ASSUME_YES=1 ;;
        --purge) PURGE=1 ;;
        --dir)
            shift
            [ $# -gt 0 ] || die "--dir needs a path"
            INSTALL_DIR="$1"
            ;;
        *) die "unknown argument '$1' (try: sh install.sh help)" ;;
    esac
    shift
done

case "$(uname -s)" in
    Darwin) ;;
    *) die "$APP_NAME is macOS-only today (this host is $(uname -s))" ;;
esac

# ── where the app goes ───────────────────────────────────────────────────────
# /Applications when we may write there, otherwise the per-user one — no sudo, ever.
app_dir() {
    if [ -n "$INSTALL_DIR" ]; then
        printf '%s' "$INSTALL_DIR"
    elif [ -w /Applications ]; then
        printf '/Applications'
    else
        printf '%s/Applications' "$HOME"
    fi
}

# Every place an installed copy could be, for update and remove — one per line, because a
# home directory may well contain a space.
installed_paths() {
    { printf '/Applications\n%s/Applications\n' "$HOME"
      [ -n "$INSTALL_DIR" ] && printf '%s\n' "$INSTALL_DIR"
      true
    } | while IFS= read -r d; do
        [ -d "$d/$APP_NAME.app" ] && printf '%s/%s.app\n' "$d" "$APP_NAME"
        true
    done
}

# ── remove ───────────────────────────────────────────────────────────────────
if [ "$CMD" = "remove" ]; then
    found=0
    apps="$(installed_paths)"
    printf '%s\n' "$apps" | while IFS= read -r app; do
        [ -n "$app" ] || continue
        if confirm "Delete $app?"; then
            rm -rf "$app"
            say "removed $app"
        else
            note "kept $app"
        fi
    done
    [ -n "$apps" ] && found=1
    [ "$found" -eq 1 ] || note "no installed $APP_NAME.app found"

    if [ -d "$SRC" ] && confirm "Delete the build directory $SRC?"; then
        rm -rf "$SRC"
        say "removed $SRC"
    fi

    settings="$HOME/.$APP_NAME"
    if [ -d "$settings" ]; then
        if [ "$PURGE" -eq 1 ] && confirm "Delete your settings in $settings?"; then
            rm -rf "$settings"
            say "removed $settings"
        else
            note "your settings are still in $settings (delete them too with: … | sh -s -- remove --purge)"
        fi
    fi
    say "done"
    exit 0
fi

# ── toolchain ────────────────────────────────────────────────────────────────
ensure_git() {
    have git && return 0
    warn "git is missing — macOS installs it with the Command Line Tools"
    if confirm "Run 'xcode-select --install' now?"; then
        xcode-select --install >/dev/null 2>&1 || true
        die "finish the Command Line Tools install, then run this again"
    fi
    die "git is required"
}

ensure_rust() {
    # A previous rustup install in this same shell session isn't on PATH yet.
    [ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
    # `cargo` on PATH isn't enough: a rustup shim with no default toolchain is on PATH
    # too, and only fails once you ask it to do something.
    if have cargo; then
        cargo --version >/dev/null 2>&1 && return 0
        die "cargo is installed but has no working toolchain — run: rustup default stable"
    fi
    say "installing Rust (rustup, the official installer)"
    if ! confirm "Install the Rust toolchain now?"; then
        die "Rust is required to build $APP_NAME"
    fi
    curl --proto '=https' --tlsv1.2 -fsSL https://sh.rustup.rs | sh -s -- -y --no-modify-path --default-toolchain stable
    [ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
    have cargo || die "Rust installed but 'cargo' is still not on PATH"
}

have curl || die "curl is required"
ensure_git

# ── source ───────────────────────────────────────────────────────────────────
# Running from inside a checkout? Build that one — it is what the caller meant.
here="$(cd "$(dirname "$0")" 2>/dev/null && pwd)" || here="$PWD"
if [ -f "$here/tools/bundle-macos.sh" ] && [ -f "$here/Cargo.toml" ]; then
    SRC="$here"
    say "building the checkout at $SRC"
else
    if [ -z "$REF" ]; then
        REF="$(latest_tag)"
        if [ -n "$REF" ]; then
            say "latest release: $REF"
        else
            # A fork with no releases yet, or a remote we could not reach. Building the
            # default branch is the useful thing to do — but say so, because "installed
            # the latest release" and "installed whatever was on main" are not the same
            # claim and nobody should have to guess which one they got.
            warn "no release tag on $REPO — building its default branch instead"
            REF="HEAD"
        fi
    fi

    if [ -d "$SRC/.git" ]; then
        say "updating $SRC to $REF"
        # Fetch the tag ITSELF when there is one, so `git describe` in this directory
        # answers "which release is this?" rather than naming an older tag the shallow
        # clone happens to still have. A branch or a commit has no tag to fetch, so that
        # first attempt just fails and the plain fetch behind it does the work.
        git -C "$SRC" fetch --quiet origin "+refs/tags/$REF:refs/tags/$REF" 2>/dev/null \
            || git -C "$SRC" fetch --quiet origin "$REF" \
            || die "could not fetch '$REF' from $REPO"
        # Check out FETCH_HEAD, not a tracking ref: a tag is not a branch, and a shallow
        # clone may keep no tracking ref anyway. Detached is right — this directory
        # follows releases, it is not somewhere to develop. Without `--force`, so local
        # work is never destroyed: the checkout fails and we build what is there.
        if ! git -C "$SRC" checkout --quiet --detach FETCH_HEAD 2>/dev/null; then
            warn "the checkout has local changes; leaving them alone and building as-is"
        fi
    else
        say "cloning $REPO at $REF"
        mkdir -p "$(dirname "$SRC")"
        if [ "$REF" = "HEAD" ]; then
            git clone --depth 1 "$REPO" "$SRC"
        else
            git clone --depth 1 --branch "$REF" "$REPO" "$SRC"
        fi
    fi
fi

ensure_rust

# ── build ────────────────────────────────────────────────────────────────────
say "building $APP_NAME (this is a from-scratch Rust build — no dependencies to fetch)"
( cd "$SRC" && sh tools/bundle-macos.sh $ARCH_ARG )

built="$SRC/dist/$APP_NAME.app"
[ -d "$built" ] || die "the build did not produce $built"

if [ "$DO_INSTALL" -eq 0 ]; then
    say "built: $built"
    note "install it with: cp -R \"$built\" /Applications/"
    exit 0
fi

# ── install ──────────────────────────────────────────────────────────────────
dest="$(app_dir)"
mkdir -p "$dest" 2>/dev/null || true
[ -w "$dest" ] || die "cannot write to $dest — try again with: --dir \"\$HOME/Applications\""

if [ -d "$dest/$APP_NAME.app" ]; then
    confirm "Replace the existing $dest/$APP_NAME.app?" || { note "kept the existing app; the new build is at $built"; exit 0; }
fi
rm -rf "$dest/$APP_NAME.app"
cp -R "$built" "$dest/$APP_NAME.app"
say "installed $dest/$APP_NAME.app"

# A fallback install is easy to lose track of, so say why it happened. An explicit
# `--dir` was the caller's own choice and needs no explanation.
if [ -z "$INSTALL_DIR" ] && [ "$dest" != "/Applications" ]; then
    note "(/Applications wasn't writable, so it went here — drag it over if you prefer)"
fi

printf '\n%sOpen it:%s     open -a "%s"\n' "$BOLD" "$RESET" "$APP_NAME"
# The same one-liner does both, which is the whole point of it.
printf '%sUpdate later:%s curl -fsSL %s | sh\n' "$BOLD" "$RESET" "$INSTALL_URL"
printf '%sRemove it:%s    curl -fsSL %s | sh -s -- remove\n\n' "$BOLD" "$RESET" "$INSTALL_URL"
