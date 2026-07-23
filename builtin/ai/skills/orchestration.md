---
describe = "Orchestrate sub-agents: delegate breadth in parallel, then synthesize."
---
You can **delegate** to focused sub-agents with the **`task.run`** tool and fold their reports
back into your own reasoning — keeping your context for decisions while they do the breadth.
Use it deliberately.

## When to delegate (vs. do it inline)
- **Delegate** when a sub-task is **wide but self-contained** and would flood your context:
  mapping an unfamiliar area, reviewing a diff, running and interpreting a test suite,
  investigating several leads at once. Each comes back as a tight report.
- **Do it inline** when the work is small, needs your full conversation context, or is the
  actual change itself. Don't delegate a one-file read or a two-line edit — that's slower.

## The sub-agents
- **`explorer`** — read-only scout. Give it a precise question ("where is X handled, and what
  calls it?"); it returns a `path:line` map. Fan out several to cover unknown areas fast.
- **`reviewer`** — read-only critique of a diff/change (correctness → security → tests → design).
- **`tester`** — runs the suite, interprets failures, can add coverage.

## How to delegate well
1. **Write a crisp, standalone prompt.** The sub-agent doesn't see your conversation — state
   the goal, the relevant paths, and exactly what to return. Vague in, vague out.
2. **Fan out in parallel.** Pass a batch — `{"tasks":[{"agent":"explorer","prompt":"…"},
   {"agent":"explorer","prompt":"…"}]}` — to investigate independent leads at once; they run
   concurrently and you get all reports together. Sequence only when one depends on another.
3. **Right tool per task.** `explorer` to learn, `reviewer` to critique, `tester` to verify.
   Sub-agents are read/safe-only — they can't make your edits; you act on their findings.
4. **Synthesize, don't paste.** Read the reports, reconcile them, and decide. State your plan
   and act — don't just forward a sub-agent's text as your answer.

A strong loop on a non-trivial task: **plan** (`todo.*`) → **delegate breadth** (`explorer`
to map, in parallel) → **make the change** yourself → **delegate verification** (`tester`,
`reviewer`) → **synthesize and report**.
