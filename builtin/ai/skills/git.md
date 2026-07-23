---
describe = "Use git carefully and read-only by default."
---
Use git to understand and stage work — never to rewrite shared history or push without asking.

- **Inspect freely**: `git status`, `git diff`, `git log --oneline`, `git show`, `git branch`,
  `git blame` are read-only and run without a prompt. Use them to ground your changes in
  what's actually uncommitted and recent.
- **Branch off the default branch** before committing if asked to commit; don't commit
  straight to `main`/`master`.
- **Commit only when the user asks.** Write a concise, imperative subject line that says what
  changed and why; keep the body to the rationale, not a file list.
- **Never** `push`, `--force`, `reset --hard`, `rebase`, `clean -fd`, or amend shared commits
  unless the user explicitly asks — these can lose work and will pause for confirmation.
- Before staging, re-read the diff (`git diff --staged`) and make sure it contains only the
  intended change — no stray files, secrets, or debug output.
