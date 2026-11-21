## Memory format (project state rules)

How agents keep project docs current. Applies to every project with `.agent/state.yaml`.

### `.agent/state.yaml` (overwrite in place, keep it ≤40 lines)
```yaml
updated: 2026-01-01          # YYYY-MM-DD
milestone: MVP               # must match a ROADMAP.md milestone
branch: feature/auth
task: "Add JWT auth middleware"   # current TODO.md item, verbatim
status: in_progress          # planned | in_progress | blocked | review | done
plan: .agent/plans/auth.md   # or null
done:                        # finished in this task, short
  - token signing + tests
next:                        # concrete next steps, in order
  - refresh-token rotation
blockers: []                 # what's blocking + who/what resolves it
notes: []                    # gotchas a fresh agent must know (not history)
```

### When to update what
| Event | Update |
|---|---|
| Start/finish a step | `state.yaml` (`status`, `done`, `next`) |
| Finish a TODO item | tick it in `TODO.md`, set `state.yaml` to the next item |
| Non-obvious choice (lib, pattern, trade-off) | append to `DESIGN_DECISIONS.md` |
| Boundary/component/data-flow change | `ARCHITECTURE.md` (+ module `AGENTS.md`) |
| Scope/requirement change | `SPEC.md` (ask me first) |
| Milestone done | `ROADMAP.md`, `CHANGELOG.md`, new `TODO.md` section |
| New command/tooling | `AGENTS.md` commands section + `Makefile` |

### Formats
- `DESIGN_DECISIONS.md`: `## D-NNN Title (YYYY-MM-DD)` then one line each for `Context:`, `Decision:`, `Why:`, `Rejected:`. Append only. To change a decision, add a new entry that says `Supersedes D-NNN`.
- `TODO.md`: `## <milestone>` then `- [ ] item` (one PR-sized item each). `- [x]` when done.
- `.agent/plans/<task>.md`: Goal, Steps (checkboxes), Files touched, Risks, Test plan. Delete the file once the task is merged.
- Never keep history in `state.yaml`. Git has the history.
