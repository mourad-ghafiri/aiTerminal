#!/usr/bin/env bash
# Unsafe-confinement gate (companion to check_no_crates.sh / check_layers.sh).
#
# INVARIANT: `unsafe` exists ONLY in the platform OS FFI module
# (crates/platform/src/os/). Every other crate root sets `#![forbid(unsafe_code)]`
# (platform sets `#![deny(unsafe_code)]` and opens the `os` module with a single
# `#[allow(unsafe_code)]`). This gate fails the build if any `#[allow(unsafe_code)]`
# escape hatch or actual unsafe construct appears outside that one module.
set -euo pipefail
cd "$(dirname "$0")/.."

OSDIR="crates/platform/src/os/"
violations=0

# 1) No `allow(unsafe_code)` outside the os module — except the single line in
#    crates/platform/src/lib.rs that opens `pub mod os`.
while IFS= read -r hit; do
  [ -n "$hit" ] || continue
  file="${hit%%:*}"
  case "$file" in
    "$OSDIR"*) continue ;;
    crates/platform/src/lib.rs) continue ;;
  esac
  echo "unsafe gate: stray allow(unsafe_code) — $hit" >&2
  violations=$((violations + 1))
done < <(grep -rn 'allow(unsafe_code)' crates --include='*.rs' || true)

# 2) No actual unsafe code constructs outside the os module.
while IFS= read -r hit; do
  [ -n "$hit" ] || continue
  file="${hit%%:*}"
  case "$file" in "$OSDIR"*) continue ;; esac
  echo "unsafe gate: unsafe construct outside platform::os — $hit" >&2
  violations=$((violations + 1))
done < <(grep -rnE '\bunsafe[[:space:]]+(fn|impl|trait|extern|\{)' crates --include='*.rs' || true)

if [ "$violations" -ne 0 ]; then
  echo "unsafe gate FAILED — $violations occurrence(s) of unsafe outside crates/platform/src/os/." >&2
  exit 1
fi
echo "unsafe gate OK — all unsafe is confined to crates/platform/src/os/."
