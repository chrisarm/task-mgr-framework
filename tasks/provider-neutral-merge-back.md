# Provider-neutral parallel merge-back

**Type**: plan-tasks lean brief  
**Branch**: `feat/provider-neutral-merge-back`  
**Task list**: `tasks/provider-neutral-merge-back.json`  
**Prompt**: `tasks/provider-neutral-merge-back-prompt.md`  
**Epic**: Provider-neutral host selection (Slice A — merge)  
**Sibling (do not fold)**: `tasks/skip-anthropic-usage-claude-disabled.*` (usage dual predicate)

## Problem

Parallel-slot merge-back always wires `ClaudeMergeResolver` and treats **every** non-zero `git merge` as a conflict-resolution problem. Real incident:

```text
merge: feat/…-slot-1 - not something we can merge
| resolver failed: no conflicts reported, refusing to spawn (likely dirty WT …)
… after Claude resolution attempt …
Aborting: 2 consecutive merge-back failure wave(s)
```

Truth: git ref unmergeable → empty unmerged paths → resolver never spawns Claude → still labeled Claude attempt; dirty-WT text is a red herring. Grok-only/Codex-only configs also cannot resolve **true** content conflicts without Claude.

## In scope

- Classify non-conflict merge failures; **do not** call resolver without content conflicts
- Honest diagnostics (no false Claude / sole dirty-WT blame)
- Provider-aware merge resolver factory (Claude / Grok / NoAgent)
- Wire live wave + FEAT-005 startup auto-recovery
- Operator recovery text for stranded `{base}-slot-N`
- Hermetic tests

## Out of scope

- Anthropic usage/OAuth dual predicate (sibling task list)
- Codex WebSocket→HTTPS transport (external `codex` CLI; docs-only elsewhere)
- Crash escalation Claude ladder (Slice C)
- Curate / PRD mutate hosts (Slice D)
- Full slot0 WIP partial-stash redesign
- Softening default halt threshold to 0

## Success bar

- `not something we can merge` → PreResolver, no resolver call, recovery printed
- True conflict + Claude enabled → Claude resolver (preserved)
- True conflict + Claude disabled + Grok enabled → Grok resolver
- `--parallel 1` unchanged

## Key files

- `src/loop_engine/worktree.rs` — prepare, merge attempt, kinds, trait
- `src/loop_engine/merge_resolver.rs` — Claude resolver, short-circuit
- `src/loop_engine/wave_scheduler.rs` — wire + warning + halt
- `src/loop_engine/CLAUDE.md` — slot merge-back section
