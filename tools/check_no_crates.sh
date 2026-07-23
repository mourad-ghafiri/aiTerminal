#!/usr/bin/env sh
# Zero-crate invariant gate.
#
# A workspace member has no `source` field in Cargo.lock; any registry/git
# dependency DOES. So if Cargo.lock contains a single `source = ...` line, a
# third-party crate has crept in (directly or transitively) and we fail.
#
# Usage: tools/check_no_crates.sh [path/to/Cargo.lock]
set -eu

LOCK="${1:-Cargo.lock}"

if [ ! -f "$LOCK" ]; then
    echo "no Cargo.lock yet at '$LOCK' — run 'cargo build' first" >&2
    exit 0
fi

# Match the TOML key exactly: a line that is `source = "..."`.
if offenders="$(grep -nE '^source = ' "$LOCK")"; then
    echo "ZERO-CRATE GATE FAILED: $LOCK references non-local package sources:" >&2
    echo "$offenders" >&2
    echo >&2
    echo "Only path/workspace dependencies are allowed. Remove the offending crate." >&2
    exit 1
fi

pkgs="$(grep -cE '^name = ' "$LOCK" || true)"
echo "zero-crate gate OK — $pkgs workspace packages, no external sources in $LOCK"
