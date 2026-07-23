---
describe = "Write and run tests the way a disciplined engineer does."
---
When adding or changing behavior, cover it with a test — and **run the suite** to confirm.

- **Find the project's test runner** before writing anything: look for `cargo test`, `npm
  test` / `vitest` / `jest`, `pytest`, `go test`, a `Makefile` target, or a `scripts.test`
  in the manifest. Match the existing test style, location, and naming.
- **Test behavior, not implementation.** Assert observable outputs and effects; avoid
  asserting private internals that will churn.
- **Cover the edges**, not just the happy path: empty/zero, boundaries, error paths,
  duplicates, and the specific bug you're fixing (a regression test that fails before your
  fix and passes after).
- **Keep tests hermetic** — no network, no real clock/randomness, no machine-global state;
  use temp dirs and fakes. A test must not mutate the user's real environment.
- **Run it.** Execute the suite with `sys.run`, read the output, and fix failures. Report the
  actual result (passed N / failed M) — never claim green without running it.
