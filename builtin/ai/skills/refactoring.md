---
describe = "Refactor safely — behavior-preserving, test-backed, incremental."
---
A refactor changes structure, **never behavior**. Do it safely:

- **Green before and after.** Make sure the tests pass first; if coverage is thin, add a
  characterization test that pins the current behavior before you touch anything.
- **Small steps.** Rename → extract → inline → move, one mechanical change at a time, running
  the tests between steps. A giant rewrite is not a refactor — it's a rewrite, and it's risky.
- **Don't mix concerns.** Keep a refactor separate from a behavior change or a bug fix; do
  one, verify, then the other. Don't sneak in unrequested "improvements".
- **Match the codebase.** Follow its existing patterns and naming; the goal is code that
  reads like the rest of the project, not your personal style.
- **Delete dead code** you make unreachable — don't leave it commented out.

Finish by confirming the suite is still green and summarizing what moved and why it's clearer.
