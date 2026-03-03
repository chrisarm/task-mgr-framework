# PRD: Enrich Learning Extraction with Full Session Conversation

**Type**: Enhancement
**Priority**: P2 (Medium)
**Author**: Claude Code
**Created**: 2026-03-02
**Status**: Draft

---

## 1. Overview

### Problem Statement

The learning extraction system receives only `--print` text output from Claude iterations. `--print` captures only the **final assistant text response** — not tool calls, file reads/writes, errors, or intermediate reasoning. Successful iterations typically end with just `<completed>task-id</completed>`, providing zero useful content for learning extraction. This means the system cannot learn from the most valuable part of an iteration: what tools were used, what errors were encountered, what workarounds were applied.

### Background

- `spawn_claude` in `src/loop_engine/claude.rs` runs `claude --print --dangerously-skip-permissions -p <prompt>` and captures stdout line-by-line
- Claude Code CLI persists full session conversations as JSONL files at `~/.claude/projects/<encoded-path>/<session-id>.jsonl`
- These JSONL files contain all messages: user prompts, assistant responses (text + tool_use blocks), tool results, thinking blocks
- The `--output-format json` flag returns a result object with a `session_id` field that can be used to locate the session file
- The `--session-id <uuid>` flag lets us pre-assign a session ID so we know the filename before the process exits
- `uuid` crate (v1, features: v4, serde) is already a dependency

---

## 2. Goals

### Primary Goals

- [ ] After each loop iteration, read the full session JSONL to provide rich context to learning extraction
- [ ] Graceful degradation: if session file is unavailable, fall back to existing `--print` output
- [ ] Prevent utility spawns (learning extraction, curation) from cluttering session storage

### Success Metrics

- Learning extraction receives output with tool call details (output len >> current ~30 chars)
- Zero impact on loop iteration speed or reliability (session read is post-hoc, best-effort)
- Utility spawns no longer create orphaned session files

---

## 2.5. Quality Dimensions

### Correctness Requirements

- Session file reader must never crash the loop — all errors return `None` with a warning log
- Per-block truncation must not corrupt JSON or split mid-character (truncate at char boundary)
- `--session-id` must be a valid UUID v4 (Claude CLI validates this)

### Performance Requirements

- Session file reading is post-hoc (after iteration completes), not on the hot path
- Per-block truncation (500 chars tool_use input, 1000 chars tool_result) prevents one large block from consuming the 50K budget
- Exit early if session file doesn't exist — don't attempt directory listing or fuzzy matching

### Style Requirements

- Follow existing `loop_engine` module patterns: public function, `TaskMgrResult` return type, `eprintln!` for warnings
- Match existing graceful degradation pattern from `src/learnings/ingestion/mod.rs` (best-effort, never crash)

### Known Edge Cases

| Edge Case | Why It Matters | Expected Behavior |
|-----------|---------------|-------------------|
| Session file missing (e.g., Claude crashed before writing) | Process may exit before flushing JSONL | Return `None`, log warning, fall back to `--print` output |
| Worktree working dir vs main repo | Session path encoding differs per working dir | Encode the actual `working_dir` passed to `spawn_claude`, not a hardcoded path |
| Session file with only thinking blocks | Some iterations produce no tool calls | Return the text blocks only; if empty, return `None` |
| Enormous tool result (e.g., Read of large file) | Single block could consume entire 50K budget | Per-block truncation to 1000 chars |
| Malformed JSONL line | Corrupt or partial write | Skip malformed lines, continue processing |
| Home dir resolution | `~` expansion differs by platform | Use `dirs::home_dir()` or `std::env::var("HOME")` |

---

## 3. User Stories

### US-001: Rich Learning Extraction from Iterations

**As a** task-mgr loop operator
**I want** learning extraction to see the full conversation (tool calls, errors, file changes)
**So that** the system can extract meaningful patterns and failure modes, not just see `<completed>` tags

**Acceptance Criteria:**

- [ ] After a loop iteration, the learning extraction prompt receives session content including tool names and truncated inputs/results
- [ ] If session file is unavailable, falls back to `--print` output without error
- [ ] Output passed to extraction is ≤50,000 chars

### US-002: Clean Utility Spawn Sessions

**As a** task-mgr operator
**I want** utility spawns (learning extraction, curation, enrichment) to not persist session files
**So that** the `~/.claude/projects/` directory doesn't fill with disposable sessions

**Acceptance Criteria:**

- [ ] Learning extraction, dedup curation, and enrichment spawns use `--no-session-persistence`
- [ ] No functional change to utility spawn behavior

---

## 4. Functional Requirements

### FR-001: Add `--session-id` to Loop Iteration Spawns

Generate a UUID v4 before spawning Claude for loop iterations. Pass `--session-id <uuid>` in the CLI args. Return the session ID in `ClaudeResult`.

**Details:**
- Only loop iteration spawns get a session ID (the engine.rs caller)
- Utility spawns (ingestion, curation, enrichment) do not get a session ID
- Approach: add a `session_id: Option<String>` parameter to `spawn_claude`. When `Some`, adds `--session-id <value>` to args. When `None`, no session-id flag.

**Validation:**
- Unit test: verify `--session-id <uuid>` appears in constructed args when provided
- Unit test: verify no `--session-id` when `None`

### FR-002: Session File Reader

Read a Claude session JSONL file and extract a text summary suitable for learning extraction.

**Details:**
- Derive session file path: `{home}/.claude/projects/{encoded_working_dir}/{session_id}.jsonl`
  - `encoded_working_dir`: the absolute path with `/` replaced by `-` (e.g., `$HOME/foo` → `-home-chris-foo`)
- Parse JSONL line by line, extract from messages of type `assistant` and `user`:
  - `assistant` text blocks → include full text
  - `assistant` tool_use blocks → format as `[Tool: {name}] {input truncated to 500 chars}`
  - `user` tool_result blocks → include content truncated to 1000 chars
  - Skip: `thinking` blocks, `queue-operation`, `progress`, `system`, `file-history-snapshot` message types
- Global truncation: cap total output at 50,000 chars
- Return `Option<String>`: `Some(content)` if successfully read, `None` on any error

**Validation:**
- Unit tests with sample JSONL data (inline test fixtures)
- Test: valid session with tool calls produces expected formatted output
- Test: missing file returns `None`
- Test: malformed JSONL lines are skipped gracefully

### FR-003: Engine Integration

After iteration completes, attempt to read the session file. Use session content for learning extraction if available, otherwise fall back to existing `--print` output.

**Details:**
- Location: `src/loop_engine/engine.rs` around line 506 (Step 7.7)
- Logic: if `claude_result.session_id` is `Some`, call session reader; if `None` or read fails, use `claude_output`

**Validation:**
- Integration: run a loop iteration and verify learning extraction log shows larger output len

### FR-004: `--no-session-persistence` for Utility Spawns

Add `--no-session-persistence` flag to utility spawn calls.

**Details:**
- Affected callers:
  - `src/learnings/ingestion/mod.rs:77` (learning extraction)
  - `src/commands/curate/mod.rs:613` (dedup curation)
  - `src/commands/curate/enrich.rs:234` (enrichment)
- Approach: add `no_session_persistence: bool` parameter to `spawn_claude`, or just have these callers construct the flag separately

**Validation:**
- Verify utility spawns don't create session files (manual check)

---

## 5. Non-Goals (Out of Scope)

- **Changing `--print` behavior** — live stderr tee display remains unchanged
- **Streaming JSON output** — `--output-format stream-json` is more complex and unnecessary
- **Session file cleanup/rotation** — separate concern for future work
- **Extracting thinking blocks** — too verbose, low signal-to-noise ratio for learnings

---

## 6. Technical Considerations

### Affected Components

- `src/loop_engine/claude.rs` — add `session_id` param and return field, `no_session_persistence` param
- `src/loop_engine/session.rs` — **NEW** session JSONL reader
- `src/loop_engine/mod.rs` — add `pub mod session;`
- `src/loop_engine/engine.rs` — pass session_id to spawn, use session content for learning extraction
- `src/learnings/ingestion/mod.rs` — pass `no_session_persistence=true` and `session_id=None`
- `src/commands/curate/mod.rs` — pass `no_session_persistence=true` and `session_id=None`
- `src/commands/curate/enrich.rs` — pass `no_session_persistence=true` and `session_id=None`

### Dependencies

- `uuid` crate (already in Cargo.toml)
- `dirs` crate or `std::env::var("HOME")` for home directory resolution
- `serde_json` for JSONL parsing (already in dependencies)

### Approaches & Tradeoffs

| Approach | Pros | Cons | Recommendation |
|----------|------|------|----------------|
| A: `--session-id` + post-hoc JSONL read | No change to live output, simple, session_id is deterministic | Path encoding is reverse-engineered, could break | **Preferred** |
| B: `--output-format stream-json` | Full data in stdout, no file I/O | Requires rewriting output parser, breaks live tee pattern, complex | Rejected |
| C: `--output-format json` result only | Simple JSON wrapper | Only contains final text, no tool calls — same problem as `--print` | Rejected |

**Selected Approach**: A — pass `--session-id <uuid>` to Claude, read the JSONL session file after iteration completes. Graceful degradation if file is missing or path encoding changes.

### Risks & Mitigations

| Risk | Impact | Likelihood | Mitigation |
|------|--------|------------|------------|
| Session path encoding changes in Claude CLI update | Learning extraction loses rich data, falls back to `--print` | Low | Graceful degradation + warning log makes breakage visible |
| Session file not fully flushed on process exit | Partial/missing JSONL data | Low | Best-effort read; partial data is still more useful than `--print` only |
| Large session files slow down post-iteration processing | Adds latency between iterations | Low | 50K char cap; JSONL parsing is streaming (line-by-line), not full load |

### Security Considerations

- Session JSONL may contain file contents, env vars, or other sensitive data from tool results
- This content is passed to the learning extraction prompt (same trust boundary as current `--print` output)
- The extraction prompt already marks content as UNTRUSTED with injection-resistant delimiters
- Extracted learnings are stored in the local DB — no new exposure surface

### Public Contracts

#### New Interfaces

| Module/Endpoint | Signature | Returns (success) | Returns (error) | Side Effects |
|----------------|-----------|-------------------|-----------------|--------------|
| `loop_engine::session::read_session_for_learnings` | `(session_id: &str, working_dir: &Path) -> Option<String>` | `Some(formatted_text)` | `None` (logged) | Reads file from `~/.claude/projects/` |

#### Modified Interfaces

| Module/Endpoint | Current Signature | Proposed Signature | Breaking? | Migration |
|----------------|-------------------|-------------------|-----------|-----------|
| `loop_engine::claude::spawn_claude` | `(prompt, signal_flag, working_dir, model, timeout)` | `(prompt, signal_flag, working_dir, model, timeout, session_id, no_session_persistence)` | Yes (compile) | Update all 4 callers |
| `loop_engine::claude::ClaudeResult` | `{exit_code, output, timed_out}` | `{exit_code, output, timed_out, session_id: Option<String>}` | Yes (compile) | All pattern matches/field accesses updated |

### Consumers of Changed Behavior

| File:Line | Usage | Impact | Mitigation |
|-----------|-------|--------|------------|
| `src/loop_engine/engine.rs:439` | Main loop iteration spawn | NEEDS UPDATE — pass `session_id=Some(uuid)`, `no_session_persistence=false` | Direct update |
| `src/learnings/ingestion/mod.rs:77` | Learning extraction spawn | NEEDS UPDATE — pass `session_id=None`, `no_session_persistence=true` | Direct update |
| `src/commands/curate/mod.rs:613` | Dedup curation spawn | NEEDS UPDATE — pass `session_id=None`, `no_session_persistence=true` | Direct update |
| `src/commands/curate/enrich.rs:234` | Enrichment spawn | NEEDS UPDATE — pass `session_id=None`, `no_session_persistence=true` | Direct update |

### Inversion Checklist

- [x] All callers identified and checked? (4 callers: engine, ingestion, curate, enrich)
- [x] Routing/branching decisions that depend on output reviewed? (detection::analyze_output still uses `--print` text, unchanged)
- [x] Tests that validate current behavior identified? (spawn_claude tests in claude.rs check arg construction)
- [x] Different semantic contexts for same code discovered? (loop iteration vs utility spawn — handled via params)

---

## 7. Open Questions

- [ ] Should `spawn_claude` take a struct/builder instead of 7 positional params? (defer to separate refactor)
- [ ] Should we check `dirs` crate availability or just use `$HOME` env var?

---

## Appendix

### Session JSONL Format

Each line is a JSON object with a `type` field:
- `queue-operation` — internal scheduling (skip)
- `user` — user message with `content` field
- `assistant` — assistant response with `content` array of blocks (`text`, `tool_use`, `thinking`)
- `progress` — streaming progress updates (skip)
- `system` — system messages (skip)
- `file-history-snapshot` — file state tracking (skip)

### Example Session Content Extraction

Input JSONL assistant message:
```json
{"type":"assistant","message":{"content":[
  {"type":"text","text":"Let me read the file."},
  {"type":"tool_use","name":"Read","input":{"file_path":"/src/main.rs"}},
  {"type":"thinking","thinking":"I need to check..."}
]}}
```

Extracted output:
```
Let me read the file.
[Tool: Read] {"file_path":"/src/main.rs"}
```
(thinking block skipped)
