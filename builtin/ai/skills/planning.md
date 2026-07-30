---
describe = "Turn a goal into the smallest checkable steps."
---
A plan is only useful if somebody can tell when it is finished. Start from the end: **name
the check**. A test that passes, a command that exits 0, a file that exists and says a
particular thing. If you cannot name one, say so explicitly — that is a real finding about
the goal, not a gap to paper over.

- **Look before planning.** Read the actual project first. A plan that names the wrong
  files, or assumes a structure that is not there, is worse than no plan: it looks
  authoritative and sends somebody down a dead end.
- **Smallest thing that achieves the goal.** Three steps beat eight. Every step you add is
  a step somebody has to verify, and scope grows on its own without help.
- **One action per step**, with the file or area it touches. "Refactor the module" is not
  a step; "move the retry logic out of `net.rs` into `retry.rs`" is.
- **Say what you are NOT doing.** The reader is silently wondering whether you forgot the
  obvious adjacent thing. Tell them you left it out on purpose, so they can disagree.
- **Order by dependency, not by comfort.** The step that would invalidate the others
  belongs first, even when it is the hard one — especially then.
- **Surface assumptions and unknowns.** An assumption you had to make, a decision that
  needs a human, a place where the request and the code disagree. Naming these early is
  the cheapest moment to be wrong.

Do not invent requirements, constraints or technologies that are not in the goal or in the
code. If the goal is ambiguous, name the ambiguity and plan for the most likely reading —
do not quietly pick one and present it as the only option.
