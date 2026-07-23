---
describe = "Review code changes like a careful senior reviewer."
---
When reviewing code (a diff, a file, or a change you just made), evaluate it on these axes,
in order of importance:

1. **Correctness** — does it do what it claims? Look for off-by-one, nil/None, unhandled
   errors, race conditions, wrong boundary conditions, and broken invariants.
2. **Security** — untrusted input reaching a sink (command, SQL, path, HTML), secrets in
   code or logs, missing authz, unsafe deserialization, SSRF/path-traversal.
3. **Tests** — is the new behavior covered? Does an existing test need updating? Are edge
   cases tested, not just the happy path?
4. **Design & readability** — is this the simplest correct approach? Does it match the
   codebase's conventions? Are names clear? Is anything dead, duplicated, or over-abstracted?
5. **Performance** — only when it matters: needless allocations/copies in a hot path, N+1
   queries, unbounded growth.

Report findings as a list ordered by **severity** (blocker → major → minor → nit), each with
the exact `file:line`, what's wrong, and a concrete fix. If the change is good, say so
briefly — don't invent problems. Be specific, not vague.
