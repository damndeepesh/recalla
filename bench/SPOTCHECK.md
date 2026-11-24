# Hand spot-check of mined facts

Twenty facts taken in a fixed order (SHA-256 of suite name + fact id), judged by reading each sentence against its commit. "Good" means a durable cause, decision or gotcha a future task could use.

| Verdict | Count | Examples |
|---|---|---|
| Good | 13 | tokio `LocalSet` starvation; ripgrep "replace the spin-loop with blocking receive"; fzf "no need to rerun cygpath, $SHELL never changes"; flask "use sqlite3.Row instead of casting to dict()" |
| Weak (true, little use) | 3 | "Fixes DeprecationWarnings in Python 3.6"; "Because of this, we get to remove one `unsafe`" (cut off from its context); "fix(zsh): use the '=~' operator" (a subject, not a cause) |
| Noise | 4 | "This is not a breaking change because ..."; "This change adds track caller to ..."; a changelog bullet list; "It was decided to not protect fzf" (no why). All four were in the `unknown` state, so they were not used as ground truth |

**Finding**: of the 16 live or stale facts, 3 were weak (about 19%). The miner now drops sentences that point back at their commit ("Because of this", "This change", "This is not a breaking") and conventional-commit subjects or bullet lines; the suites were regenerated (548 -> 506 facts, 254 -> 240 probes). A fresh sample is worth taking after any miner change; this file is a snapshot, not a guarantee.

Probes were checked by reading each task against its required facts: a required fact always shares a file with the task, but sharing a file does not mean the fact would change how the task is done. Required sets therefore reward finding same-file knowledge, not only decisive knowledge.
