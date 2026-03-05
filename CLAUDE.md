# task-mgr Project Notes

## Database Location

The Ralph loop database is at `.task-mgr/tasks.db` (relative to the project/worktree root). Each worktree has its own copy.

## Worktrees

- Main: `$HOME/projects/task-mgr`
- Feature worktrees: `$HOME/projects/task-mgr-worktrees/<branch-name>/`

## Task Files

- PRD task lists: `.task-mgr/tasks/<prd-name>.json`
- Loop prompts: `.task-mgr/tasks/<prd-name>-prompt.md`
- Progress log: `.task-mgr/tasks/progress.txt`
