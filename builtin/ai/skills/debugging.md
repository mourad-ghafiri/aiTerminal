---
describe = "Debug methodically — reproduce, isolate, fix the root cause."
---
When something is broken, don't guess-and-patch. Work the problem:

1. **Reproduce** it reliably first — find the smallest input/command that triggers it. If you
   can't reproduce it, you can't confirm a fix.
2. **Read the actual error** — the full message, stack trace, and the failing line. Run the
   failing test/command with `sys.run` and look at real output, not assumptions.
3. **Isolate** — `fs.search` for the symbol, trace the data flow, narrow to the smallest code
   region that's wrong. Form one hypothesis at a time and test it.
4. **Fix the root cause**, not the symptom. A `try/catch` that hides the error, or a special
   case that masks it, is not a fix.
5. **Confirm** — re-run the repro and the suite. Add a regression test that would have caught
   this. State what the bug was, why it happened, and how the fix addresses it.

Prefer the smallest change that fixes the actual cause. If you're uncertain, add logging,
reproduce, then remove it.
