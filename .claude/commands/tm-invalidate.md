# /tm-invalidate - Invalidate a Wrong Learning

Degrade a learning that turned out to be wrong or harmful via two-step degradation.

## Usage

```
/tm-invalidate <learning-id>
/tm-invalidate                    # Search first, then invalidate
```

## Instructions

### If ID Provided

Run the invalidation directly:

```bash
task-mgr invalidate-learning <id>
```

Explain the result:
- **"downgraded"**: Confidence was set to Low. The learning is still visible but deprioritized. Call again to retire it completely.
- **"retired"**: The learning was already Low confidence and is now soft-archived. It will no longer appear in recall results.

### If No ID Provided

Help the user find the learning to invalidate:

1. Ask what the wrong learning was about
2. Search for it:
   ```bash
   task-mgr --format json recall --query "..." --limit 10
   ```
3. Show candidates with IDs
4. Confirm which one to invalidate
5. Run `task-mgr invalidate-learning <id>`

### Two-Step Behavior

| Call | Starting State | Result |
|------|---------------|--------|
| First | High/Medium confidence | Downgrades to Low |
| First | Low confidence | Retires (soft-archive) |
| Second | Low confidence (from prior downgrade) | Retires |
| Any | Already retired | Error |

### When to Use

- A recalled learning gave wrong advice that caused a bug
- A pattern learning describes an approach that doesn't actually work
- A workaround is no longer needed (underlying issue was fixed)
- A failure learning misidentifies the root cause
