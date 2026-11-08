@AGENTS.md

## Claude Code notes

- `AGENTS.md` above is the canonical guide; change it there, not here.
- Worktrees made by Claude Code live in `.claude/worktrees/` (gitignored). Build in
  them with `INK_MUPDF_CACHE` pointing at the main checkout's `target/mupdf-cache`.
- Never use a bare `git stash`: the stash stack is shared with other worktrees.
  Set work aside with a WIP commit instead.
- Write scratch output (sim PNGs, logs, copied binaries) to your scratchpad or to
  `out/` (gitignored), never into `testdata/` or `docs/`.
