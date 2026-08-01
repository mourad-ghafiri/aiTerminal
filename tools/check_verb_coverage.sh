#!/usr/bin/env bash
# Verb-coverage gate (companion to check_no_crates.sh / check_layers.sh /
# check_unsafe.sh / check_file_size.sh).
#
# INVARIANT: every verb a command documents in its own usage text is executed by at
# least one scenario, as typed.
#
# This gate exists because of the specific gap it was written to close. Run against the
# tree the day it was added, it named five: `@flow watch`, and the whole `@loop` reading
# surface — `show`, `log`, `resume`, `clear`. All four `@loop` verbs were documented,
# implemented and unit-tested at the engine, and no scenario had ever typed one. The
# dispatch in between (route the verb, resolve the id, find the record, refuse when
# there is none) was proved by nothing. That is invisible in a coverage report, because
# the code IS covered; what is missing is the path a person takes to it.
#
# The usage text is the source of truth on purpose. It is what a command promises when
# somebody is stuck, so a verb that is advertised and unproven is exactly the pair this
# should refuse — and a verb that is quietly dropped from the usage stops being claimed
# and stops being required here, in the same edit.
#
# What it does NOT prove, so that nobody reads a pass for more than it is: that the verb
# was run against a record that EXISTS. `@flow show last` on an empty store satisfies
# this gate and only proves the refusal. Reading a real run back is scenario 100's job,
# and no script can check that a scenario was worth writing.
#
# Usage: tools/check_verb_coverage.sh
set -euo pipefail
cd "$(dirname "$0")/.."

scenarios=$(cat scenarios/**/*.toml scenarios/*.toml 2>/dev/null || true)
missing=0
checked=0

# Each usage line reads `       @flow show <id>   …`. Take the command and the verb
# word; `show|log|resume` on one line documents three verbs, so split on `|`.
while IFS= read -r pair; do
  cmd=${pair%% *}
  verbs=${pair#* }
  IFS='|' read -ra each <<< "$verbs"
  for verb in "${each[@]}"; do
    checked=$((checked + 1))
    # As typed: `run = "@flow show …"`. The trailing character keeps `node` from being
    # satisfied by `nodes`.
    if ! grep -qE "run = \"@${cmd} ${verb}([ \"]|\$)" <<< "$scenarios"; then
      echo "verb gate: @${cmd} ${verb} is documented but no scenario runs it" >&2
      missing=$((missing + 1))
    fi
  done
done < <(grep -rhoE '"[[:space:]]+@(flow|loop|job) [a-z|]+' \
  crates/framework/src/cli/flow/args.rs \
  crates/framework/src/cli/agentloop/args.rs \
  crates/framework/src/cli/jobs/create.rs \
  | sed -E 's/^"[[:space:]]+@//' | sort -u)

if [ "$missing" -ne 0 ]; then
  echo >&2
  echo "verb gate FAILED — $missing documented verb(s) that no scenario executes." >&2
  echo "Add a step to a scenario in scenarios/cli/, or take the verb out of the usage text." >&2
  exit 1
fi
echo "verb gate OK — $checked documented verbs, every one executed by a scenario."
