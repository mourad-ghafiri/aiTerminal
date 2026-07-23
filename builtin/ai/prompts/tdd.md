---
description = "Steer the agent to work test-first (red → green → refactor)."
---
Work test-first. For the change requested:
1. Write a failing test that specifies the desired behavior (red) — run it and confirm it
   fails for the right reason.
2. Write the minimal code to make it pass (green) — run the suite and confirm.
3. Refactor for clarity with the tests still green.
State each step and its run result; never claim green without running the suite.
