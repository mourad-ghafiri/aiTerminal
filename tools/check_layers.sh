#!/usr/bin/env bash
# Strict layered-architecture gate (companion to check_no_crates.sh).
#
# The workspace is four layers — core < platform < framework < app — each ONE
# crate with internal modules and no facade (`corelib`, `platform`, `framework`,
# `app`). INVARIANT: a lower layer may name any strictly-lower layer it uses
# directly (with no facades there are no re-exports, so e.g. framework -> platform
# AND corelib); same-layer sibling deps are allowed; and the Platform testkit may be
# a dev-dependency anywhere. The TOP layer is tighter: `app` is the thin
# composition root and may depend on `framework` ONLY — framework re-exposes whatever
# it needs, so app never names corelib/platform. Any other edge — an upward dep,
# a skipped layer, or app reaching past framework — fails the build, the same way
# tools/check_no_crates.sh fails on a third-party crate.
set -euo pipefail

cd "$(dirname "$0")/.."

# Layer index by crate name; facade crate of each layer.
layer_of() {
  case "$1" in
    app) echo 3 ;;
    framework|framework-*) echo 2 ;;
    platform|platform-*) echo 1 ;;
    corelib|core-*) echo 0 ;;
    *) echo "-1" ;;
  esac
}
facade_of() { # facade crate name for a layer index
  case "$1" in
    0) echo corelib ;;
    1) echo platform ;;
    2) echo framework ;;
    *) echo "" ;;
  esac
}

violations=0
checked=0

for toml in crates/*/Cargo.toml; do
  [ -f "$toml" ] || continue
  crate=$(awk -F' *= *' '/^name *=/{gsub(/"/,"",$2); print $2; exit}' "$toml")
  lc=$(layer_of "$crate")
  if [ "$lc" = "-1" ]; then
    echo "layer gate: UNKNOWN layer for crate '$crate' ($toml)" >&2
    violations=$((violations + 1))
    continue
  fi
  checked=$((checked + 1))

  # Walk the manifest, tracking whether we're in [dev-dependencies], and inspect
  # every path-dependency line ("<dep> = { path = ... }").
  section=""
  while IFS= read -r line; do
    case "$line" in
      "["*"]") section="$line" ;;
    esac
    case "$line" in
      *'path = "../'*)
        dep=$(printf '%s' "$line" | sed -E 's/^[[:space:]]*([A-Za-z0-9_-]+)[[:space:]]*=.*/\1/')
        [ -n "$dep" ] || continue
        ld=$(layer_of "$dep")
        is_dev=0
        [ "$section" = "[dev-dependencies]" ] && is_dev=1

        # OK: same-layer sibling.
        if [ "$ld" = "$lc" ]; then continue; fi

        # TOP layer is special: `app` (layer 3) may depend on `framework` ONLY.
        # It is the thin composition root — it must not name corelib/platform at all.
        if [ "$crate" = "app" ]; then
          if [ "$dep" = "framework" ]; then continue; fi
          echo "layer gate: VIOLATION — 'app' (layer 3) depends on '$dep' (layer $ld); it may depend on 'framework' ONLY." >&2
          violations=$((violations + 1))
          continue
        fi

        # OK: any strictly-lower layer's facade crate (= that layer's single crate
        # once collapsed). With no facades there are no re-exports, so a layer names
        # every lower layer it actually uses directly (framework -> platform AND corelib).
        if [ "$ld" -ge 0 ] && [ "$ld" -lt "$lc" ] && [ "$dep" = "$(facade_of "$ld")" ]; then continue; fi

        echo "layer gate: VIOLATION — '$crate' (layer $lc) depends on '$dep' (layer $ld)" >&2
        echo "            allowed: same-layer siblings, or any lower-layer facade (corelib/platform/framework)." >&2
        violations=$((violations + 1))
        ;;
    esac
  done < "$toml"
done

if [ "$violations" -ne 0 ]; then
  echo "layer gate FAILED — $violations cross-layer violation(s)." >&2
  exit 1
fi
echo "layer gate OK — $checked crates, every cross-layer edge is a lower-layer facade."
