#!/usr/bin/env bash
# File-size gate (companion to check_no_crates.sh / check_layers.sh / check_unsafe.sh).
#
# INVARIANT: no source file in `crates/` exceeds 1000 lines.
#
# Not a style preference — a contribution barrier. A 6000-line file is one nobody
# reads end to end, so nobody can be sure a change to it is safe, so the file grows.
# The limit forces the split to happen while it is still cheap: a module that outgrows
# it is one that has taken on a second job, and the fix is to give that job its own
# file. Tests count towards nothing here because they live in `<module>/tests/`,
# where the same limit applies to each of them.
#
# Usage: tools/check_file_size.sh [max-lines]
set -euo pipefail
cd "$(dirname "$0")/.."

MAX="${1:-1000}"
violations=0
checked=0
worst=0
worst_file=""

while IFS= read -r file; do
  n=$(wc -l < "$file" | tr -d ' ')
  checked=$((checked + 1))
  if [ "$n" -gt "$worst" ]; then
    worst=$n
    worst_file="$file"
  fi
  if [ "$n" -gt "$MAX" ]; then
    echo "file-size gate: $file is $n lines (limit $MAX)" >&2
    violations=$((violations + 1))
  fi
done < <(find crates -name '*.rs' -not -path '*/target/*' | sort)

if [ "$violations" -ne 0 ]; then
  echo >&2
  echo "file-size gate FAILED — $violations file(s) over $MAX lines." >&2
  echo "Split the module along a boundary it already has, and move its tests to <module>/tests/." >&2
  exit 1
fi
echo "file-size gate OK — $checked .rs files, largest is $worst lines ($worst_file), limit $MAX."
