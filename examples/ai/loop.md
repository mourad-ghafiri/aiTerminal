# `@loop` recipes — engineered agent loops

`@loop` iterates an agent until a **verifiable goal** is met: a real verifier, structured
feedback between iterations, bounds on every axis, and state you can read and resume.

## The workhorse: loop until the tests pass

```text
@loop "make the config tests pass after the schema change" --check "cargo test -p framework config::"
```

Each iteration: the `coder` agent works the goal → the check runs → its failure output feeds
the next iteration. Exit 0 from the check ends the loop.

The check runs **once before iteration 1** too. So if the tests already pass, this costs
nothing and says so; and if they don't, the maker's first attempt starts from the real failure
instead of guessing at it.

## No `--check`? The AI writes one

```text
@loop "make the config tests pass"
🔁 loop 'coder' — up to 5 iteration(s)
  verifier: cargo test -p framework config:: — proposed from the goal
```

The model reads the goal once and proposes a command whose exit status decides the goal. It is
printed before anything runs and still goes through the command guard — a "verifier" that
deploys or pushes is a side effect, not a measurement, and is refused.

For a goal nothing can measure, an independent `reviewer` agent grades each iteration instead
(`VERDICT: PASS` / `VERDICT: CONTINUE` + gaps). `--no-check` asks for that on purpose:

```text
@loop "tighten the error messages in the CLI to be actionable" --no-check
```

## Bound it on every axis

```text
@loop "finish the refactor in src/parser.rs" --check "cargo check" --max 8 --timeout 20m
```

Iterations, tokens and wall clock are three different ways to run away. `--max N` (default 5),
`--budget TOKENS`, `--timeout 30m` — and a value that can't be read is an error, not a silent
default.

## Long runs: background it, then read it

```text
@loop --bg "eliminate every clippy warning" --check "cargo clippy -- -D warnings" --max 15 --budget 500000
@job                     # ▶ running / ✓ done / ✗ failed
@loop                    # the loop's own record: verifier, iterations, outcome
@loop log last -f        # the newest iteration, live
```

## Pick it back up

```text
@loop show last                       # goal, verifier, bounds, what was already tried
@loop resume last                     # continue with what's left of each bound
@loop resume last --budget 200000     # …or with more rope
```

A run stopped by Ctrl+C, a timeout or the iteration cap resumes from where it stopped — the
attempt log goes with it, so the next iteration doesn't rediscover a dead end.

## Gate a push on a green loop

```text
@loop "make clippy clean" --check "cargo clippy -- -D warnings" && git push
```

Exit codes: `0` goal reached · `1` a bound stopped it · `2` setup error · `130` interrupted.

## The bounds you get for free

- **Success** — the check exits 0 (or the reviewer passes it).
- **`--max N`** — the iteration cap.
- **`--budget TOKENS`** · **`--timeout 30m`** — spend and wall clock.
- **No progress** — the loop remembers its last few verifier observations, so repeating *and*
  oscillating between two bad states both count. The first time, the maker gets one more
  iteration and is asked for a materially different approach; the second time ends the run.
- **The guard** — a check command the guard denies never runs, and is caught before the first
  iteration is paid for.
