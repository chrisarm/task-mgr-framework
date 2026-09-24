# Changelog — 2026-09-23

## Loop stop locations

**Branch**: `feat/loop-stop-locations`
**PRD**: `tasks/loop-stop-locations.md`

### What shipped

`task-mgr loop stop --prefix` writes the stop file for the running loop. The loop also notices a prefix stop file created after process start in the launch directory, the feature worktree, or the main checkout. A prefix file that is already there in those extra directories is deleted at start, and the message says how to create a new one. A batch that stops because of `.task-mgr/tasks/.stop` removes that file before it returns.

### Why it matters

Operators were creating `tasks/.stop-<prefix>` in the main checkout while the loop was watching the worktree, so the loop kept iterating. The command no longer depends on guessing that directory.

### Breaking changes

None

---
