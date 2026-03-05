# PRD: Enrich Learning Extraction with Full Conversation via Stream-JSON

**Type**: Enhancement
**Priority**: P2 (Medium)
**Author**: Claude Code
**Created**: 2026-03-02
**Status**: Draft

---

## 1. Overview

### Problem Statement

The learning extraction system receives only `--print` text output from Claude iterations. `--print` captures only the **final assistant text response** — not tool calls, file reads/writes, errors, or intermediate reasoning. Successful iterations typically end with just `<completed>task-id</completed>`, providing zero useful content for learning extraction.

### Background

- `spawn_claude` in `src/loop_engine/claude.rs` runs `claude --print --dangerously-skip-permissions -p <prompt>` and captures stdout line-by-line
- Claude Code CLI supports `--output-format stream-json` which emits one JSON object per line on stdout, including all assistant messages, tool calls, tool results, and a final `result` object
- The `result` object's `result` field contains the same text as `--print` output
- Using `--no-session-persistence` prevents session files from being written to `~/.claude/projects/`

---

## 2. Goals

### Primary Goals

- [ ] Capture full conversation (tool calls, errors, results) during loop iterations for learning extraction
- [ ] No session files written — keep `~/.claude/projects/` clean
- [ ] Maintain live stderr display of assistant text during iteration

### Success Metrics

- Learning extraction receives output with tool call details (output len >> current ~30 chars)
- Zero session files created by any spawn_claude call
- Live display still shows assistant text output during iteration

---

## 2.5. Quality Dimensions

### Correctness Requirements

- Stream-json parser must never crash the loop — malformed lines are skipped
- `detection::analyze_output` must receive the same text it currently gets (the `result` object's `result` field)
- Per-block truncation must not split mid-character (truncate at char boundary)

### Performance Requirements

- Stream-json parsing happens inline (same tee loop as current), not post-hoc
- Per-block truncation (500 chars tool_use input, 1000 chars tool_result) prevents one large block from consuming the 50K budget
- Live display must not buffer — text should appear as soon as each line is read

### Style Requirements

- Follow existing `loop_engine` module patterns
- Match existing graceful degradation pattern from `src/learnings/ingestion/mod.rs`

### Known Edge Cases

| Edge Case | Why It Matters | Expected Behavior |
|-----------|---------------|-------------------|
| Malformed JSON line in stream | Claude CLI bug or partial flush | Skip line, continue processing |
| Stream with only `result` type (no assistant messages) | Very short/errored iteration | Use `result.result` text as fallback (same as current behavior) |
| Enormous tool result (e.g., Read of large file) | Single block could consume entire 50K budget | Per-block truncation to 1000 chars |
| `result` object missing or `result` field is null | Error/crash scenarios | Treat as empty output for detection |
| `error` field set on assistant message | Rate limit, auth failure, etc. | Include error info in learning extraction content |
| Unicode in tool results | Truncation must not split mid-character | Use char boundary truncation |
| Utility spawns (learning extraction, curation) | These still use `--print` (text output) | Keep `--print` for utility spawns, only loop iterations use stream-json |

---

## 3. User Stories

### US-001: Rich Learning Extraction from Iterations

**As a** task-mgr loop operator
**I want** learning extraction to see the full conversation (tool calls, errors, file changes)
**So that** the system can extract meaningful patterns and failure modes

**Acceptance Criteria:**

- [ ] Loop iteration spawns use `--output-format stream-json --no-session-persistence` instead of `--print`
- [ ] Learning extraction receives formatted content including tool names and truncated inputs/results
- [ ] Output passed to extraction is ≤50,000 chars
- [ ] `detection::analyze_output` still receives the final text result (from `result` JSON object)
- [ ] Live stderr display shows assistant text as it streams

### US-002: No Session File Clutter

**As a** task-mgr operator
**I want** no session files created by any spawn_claude call
**So that** `~/.claude/projects/` stays clean

**Acceptance Criteria:**

- [ ] Loop iteration spawns use `--no-session-persistence`
- [ ] Utility spawns (learning extraction, curation, enrichment) use `--no-session-persistence`

---

## 4. Functional Requirements

### FR-001: Switch Loop Iterations to Stream-JSON Output

Change `spawn_claude` to support `--output-format stream-json --no-session-persistence` mode for loop iterations.

**Details:**
- Add a `stream_json: bool` parameter to `spawn_claude`
- When `stream_json` is true: use `--output-format stream-json --no-session-persistence` instead of `--print`
- When false: keep existing `--print` behavior (for utility spawns)
- In the tee loop, when `stream_json` is true:
  - Parse each stdout line as JSON
  - For `assistant` messages: extract text blocks → tee to stderr; collect tool_use blocks formatted as `[Tool: {name}] {truncated input}`; skip thinking blocks
  - For `user` messages: collect tool_result content (truncated)
  - For `result` messages: extract `result` field as the "print output" (for detection)
  - Skip `system` type messages
  - Collect formatted content into a separate `conversation` buffer
- `ClaudeResult` gets a new field: `conversation: Option<String>` — the formatted full conversation (only populated when `stream_json` is true)
- `ClaudeResult.output` still contains the final text result (from `result.result`) for backward compatibility with `detection::analyze_output`

**Validation:**
- Unit test: stream-json mode parses assistant + tool_use + result lines correctly
- Unit test: text mode (stream_json=false) behaves identically to current behavior
- Unit test: malformed JSON lines are skipped without panic

### FR-002: Stream Content Formatter

Parse stream-json lines and produce formatted text suitable for learning extraction.

**Details:**
- Assistant text blocks → include full text
- Assistant tool_use blocks → format as `[Tool: {name}] {json-serialized-input truncated to 500 chars}`
- User tool_result content → truncate to 1000 chars
- Skip: thinking blocks, system messages
- Include assistant `error` field if present: `[Error: {error}]`
- Global truncation: cap total formatted output at 50,000 chars

**Validation:**
- Unit tests with sample stream-json lines (inline fixtures)
- Test: formatting produces expected output for various message types

### FR-003: Engine Integration

Pass `stream_json=true` for loop iteration spawns. Use `conversation` field for learning extraction.

**Details:**
- Engine.rs: pass `stream_json=true` to `spawn_claude` for loop iterations
- After iteration: use `claude_result.conversation` for learning extraction if available, fall back to `claude_result.output`
- `detection::analyze_output` continues to use `claude_result.output` (the final text result)

**Validation:**
- Integration: run a loop iteration and verify learning extraction receives richer content

### FR-004: `--no-session-persistence` for All Spawns

All spawn_claude calls should use `--no-session-persistence`.

**Details:**
- Loop iteration spawns: already included via stream-json mode (FR-001)
- Utility spawns: add `--no-session-persistence` to args when `stream_json=false`
- This means ALL spawns get `--no-session-persistence` — simplify to always include it

**Validation:**
- Verify no session files are created by any spawn

---

## 5. Non-Goals (Out of Scope)

- **Streaming partial messages** — `--include-partial-messages` is unnecessary for our use case
- **Extracting thinking blocks** — too verbose, low signal-to-noise ratio
- **Session file cleanup/rotation** — no longer relevant since we don't create session files
- **Changing utility spawn output format** — utility spawns keep `--print` (they just need the final text)

---

## 6. Technical Considerations

### Affected Components

- `src/loop_engine/claude.rs` — modify `spawn_claude` for stream-json mode, update tee loop, update `ClaudeResult`
- `src/loop_engine/mod.rs` — no changes needed (no new module)
- `src/loop_engine/engine.rs` — pass `stream_json=true`, use `conversation` for learning extraction
- `src/learnings/ingestion/mod.rs` — add `--no-session-persistence` to utility spawn
- `src/commands/curate/mod.rs` — add `--no-session-persistence` to utility spawn
- `src/commands/curate/enrich.rs` — add `--no-session-persistence` to utility spawn

### Dependencies

- `serde_json` for JSON parsing (already in dependencies)

### Approaches & Tradeoffs

| Approach | Pros | Cons | Recommendation |
|----------|------|------|----------------|
| A: `--output-format stream-json` | Full data in stdout, no file I/O, no session files, no reverse-engineered paths | Requires modifying tee loop to parse JSON | **Preferred** |
| B: `--session-id` + post-hoc JSONL read | No change to live output | Path encoding is reverse-engineered, session files clutter disk | Rejected |
| C: `--output-format json` result only | Simple | Only contains final text, no tool calls | Rejected |

**Selected Approach**: A — use `--output-format stream-json --no-session-persistence` for loop iterations. Parse JSON lines in the tee loop, extract text for display, collect full conversation for learning extraction.

### Risks & Mitigations

| Risk | Impact | Likelihood | Mitigation |
|------|--------|------------|------------|
| stream-json format changes in Claude CLI update | Parse errors, degraded output | Low | Graceful degradation: skip unparseable lines, fall back to result text |
| JSON parsing adds latency to tee loop | Slower live display | Low | `serde_json::from_str` on single lines is fast (<1ms) |
| Large stream overwhelms conversation buffer | Memory usage spike | Low | 50K char cap on conversation buffer |

### Security Considerations

- Stream content may contain file contents, env vars, or sensitive data from tool results
- Same trust boundary as current `--print` output path
- Extraction prompt already marks content as UNTRUSTED with injection-resistant delimiters

### Public Contracts

#### Modified Interfaces

| Module/Endpoint | Current Signature | Proposed Signature | Breaking? | Migration |
|----------------|-------------------|-------------------|-----------|-----------|
| `loop_engine::claude::spawn_claude` | `(prompt, signal_flag, working_dir, model, timeout)` | `(prompt, signal_flag, working_dir, model, timeout, stream_json)` | Yes (compile) | Update all 4 callers |
| `loop_engine::claude::ClaudeResult` | `{exit_code, output, timed_out}` | `{exit_code, output, timed_out, conversation: Option<String>}` | Yes (compile) | All pattern matches updated |

### Consumers of Changed Behavior

| File:Line | Usage | Impact | Mitigation |
|-----------|-------|--------|------------|
| `src/loop_engine/engine.rs:439` | Main loop iteration spawn | NEEDS UPDATE — pass `stream_json=true` | Direct update |
| `src/learnings/ingestion/mod.rs:77` | Learning extraction spawn | NEEDS UPDATE — pass `stream_json=false` | Direct update |
| `src/commands/curate/mod.rs:613` | Dedup curation spawn | NEEDS UPDATE — pass `stream_json=false` | Direct update |
| `src/commands/curate/enrich.rs:234` | Enrichment spawn | NEEDS UPDATE — pass `stream_json=false` | Direct update |

### Stream-JSON Line Format

Each stdout line is a JSON object with a `type` field:

```json
// Assistant message with content blocks
{"type":"assistant","message":{"content":[
  {"type":"text","text":"Let me read the file."},
  {"type":"tool_use","id":"toolu_abc","name":"Read","input":{"file_path":"/src/main.rs"}},
  {"type":"thinking","thinking":"..."}
]},"model":"claude-sonnet-4-6","error":null}

// User message (tool results)
{"type":"user","message":{"content":[
  {"type":"tool_result","tool_use_id":"toolu_abc","content":"fn main() {...}","is_error":false}
]}}

// Final result (always last line)
{"type":"result","subtype":"success","result":"<completed>TASK-ID</completed>","session_id":"...","total_cost_usd":0.04,...}

// System init (skip)
{"type":"system","subtype":"init","data":{...}}
```

### Formatted Conversation Example

Input stream produces:
```
Let me read the file.
[Tool: Read] {"file_path":"/src/main.rs"}
[Result] fn main() {...}
```

---

## 7. Open Questions

- [ ] Should `spawn_claude` take a struct/builder instead of 6 positional params? (defer to separate refactor)

---

## Appendix

### Comparison: Current vs New Behavior

| Aspect | Current (`--print`) | New (`stream-json`) |
|--------|-------------------|---------------------|
| stdout content | Final text only | JSON lines with full conversation |
| Live display | Tee each line to stderr | Parse JSON, extract text, tee to stderr |
| `ClaudeResult.output` | Final text | Final text (from `result.result`) |
| Learning extraction input | Final text (~30 chars) | Full conversation (~10K-50K chars) |
| Session files | Created by default | None (`--no-session-persistence`) |
| Utility spawns | `--print` | `--print --no-session-persistence` |
