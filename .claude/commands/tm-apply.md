# /tm-apply - Confirm a Learning Was Useful

Mark a learning as applied (useful) so the UCB bandit ranking algorithm promotes it in future recalls.

## Usage

```
/tm-apply <learning-id>
/tm-apply                    # Search first, then apply
```

## Instructions

### If ID Provided

Run the apply command directly:

```bash
task-mgr apply-learning <id>
```

Explain the result:
- **times_applied**: Total lifetime applications — higher means consistently useful
- **window_applied**: Applications in current ranking window — directly boosts UCB score for next recall

### If No ID Provided

Help the user find the learning to apply:

1. Ask what learning was useful
2. Search for it:
   ```bash
   task-mgr --format json recall --query "..." --limit 10
   ```
3. Show candidates with IDs
4. Confirm which one to apply
5. Run `task-mgr apply-learning <id>`

### When to Use

- A recalled learning helped you complete a task correctly
- A pattern learning saved you from repeating a mistake
- A workaround learning helped you bypass a known issue
- A failure learning warned you away from a broken approach

### Effect on Ranking

Applying a learning:
- Increments `times_applied` (lifetime counter)
- Increments `window_applied` (current window counter)
- Updates `last_applied_at` timestamp
- All three feed the UCB bandit formula, boosting this learning's recall priority
