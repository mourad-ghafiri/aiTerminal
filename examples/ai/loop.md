# `@loop` recipes — engineered agent loops

`@loop` iterates an agent until a **verifiable goal** is met, applying the loop
engineering discipline: a real verifier, structured feedback between iterations,
and hard stop rules (success, iteration cap, no-progress, token budget).

## The workhorse: loop until the tests pass

```text
@loop "make the config tests pass after the schema change" --check "cargo test -p framework config::"
```

Each iteration: the `coder` agent works the goal → the check command runs → its
failure output feeds the next iteration. Exit 0 from the check ends the loop.

## Loop until it compiles, bounded

```text
@loop "finish the refactor in src/parser.rs" --check "cargo check" --max 8
```

## No deterministic check? Use the maker/checker split

```text
@loop "tighten the error messages in the CLI to be actionable"
```

Without `--check`, a **separate reviewer agent** grades each iteration against the
goal (`VERDICT: PASS` / `VERDICT: CONTINUE` + concrete gaps). The model that did
the work never grades its own homework.

## Long runs: background it, cap the spend

```text
@loop --bg "eliminate every clippy warning" --check "cargo clippy -- -D warnings" --max 15 --budget 500000
@job                     # ▶ running / ✓ done / ✗ failed + the log path
tail -f ~/.aiTerminal/ai/jobs/<id>/log.md
```

## Stop rules you get for free

- **Success** — the check exits 0 (or the reviewer passes it).
- **`--max N`** — iteration cap (default 5).
- **Stalled** — identical verifier output on two consecutive iterations means no
  progress; the loop stops instead of burning tokens.
- **`--budget TOKENS`** — a hard total-token ceiling.
- **The guard** — a `--check` command that the command guard denies never runs.
