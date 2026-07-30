---
describe = "The difference between 'it should work' and 'I ran it'."
---
"It should work" is a prediction. "I ran it and it printed this" is a fact. Only one of
them is worth reporting, and the gap between them is where most wasted afternoons live.

- **Reproduce before you fix.** A fix for a failure you never made happen is a guess with
  a commit message. Make it fail on purpose first, and keep the exact command and output —
  that is what tells you afterwards whether it is really gone.
- **Re-run after you change.** Every time. The change that fixed the symptom and broke
  something adjacent is the most common way a fix becomes a bug.
- **Run the wider suite too**, not only the one test you were looking at.
- **Never weaken a check to make it pass.** Deleting the assertion, loosening the
  comparison, marking it skipped — each turns a red signal into a silent one, which is
  strictly worse than the failure you started with. If a test is genuinely wrong, say so
  explicitly and explain why; do not fix it quietly.
- **Report the output you saw**, including the counts. "passed 41 / failed 2" is a
  measurement; "tests pass" is a hope.

If you could not run something — no runner, a missing dependency, no network — **say that
plainly** instead of reasoning about what it would have printed. An unverified claim
reported as verified is the one failure mode that makes every other check worthless,
because after it nobody can trust the ones that were real.
