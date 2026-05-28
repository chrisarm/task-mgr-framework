# Claude Code Agent Instructions

You are an autonomous coding agent implementing **Panic-safe stdout writes (EAGAIN on non-blocking pipe)** for **task-mgr**.

## Problem Statement

`task-mgr curate dedup` crashed at the end of an otherwise-successful run with `failed printing to stdout: Resource temporarily unavailable` (EAGAIN / os error 11).

Root cause (four-link chain, validated against the code):

1. The invocation piped through `2>&1 | tee`, which `dup2(1,2)`s task-mgr's fd 1 and fd 2 onto the **same** open file description (the pipe to `tee`).
2. `curate dedup` spawns `claude` children in the **non-PTY** branch (`use_pty` defaults to `false`), which sets `stderr(Stdio::inherit())` (`src/loop_engine/runner.rs:576`). The child inherits the parent's fd 2 → the shared OFD from step 1.
3. `claude` is Node/libuv, which marks inherited pipe stdio `O_NONBLOCK` and never restores it. `O_NONBLOCK` lives on the OFD, so task-mgr's own stdout silently becomes non-blocking.
4. After `curate_dedup()` returns, `output_result` does a single bare `print!("{}", report)` of a ~19k-line string (`src/handlers.rs:151`). The pipe fills, the now-non-blocking write returns EAGAIN, and bare `print!` has no error handling → panic. (Exit code was masked to 0 by `tee` without `pipefail`.)

The panic is strictly **downstream** of all work — `curate_dedup()` had already returned, so every LLM judgment and DB merge committed before the print. The fix is purely about not panicking on the output write, and applies at the centralized output chokepoint so **every** command is protected.

The same bare-print vulnerability is centralized at three spots in `src/handlers.rs`: `output_result` text (:151), `output_migrate_result` (:166), and `output_json` (:200). One helper fixes all three.

---

## Non-Negotiable Process (Read Every Iteration)

Before writing code:

1. **Internalize quality targets** — Read `qualityDimensions`; that's what "done well" means for THIS task.
2. **Plan edge-case handling** — For each `edgeCases` / `failureModes` entry on the task, decide how it'll be handled before coding.
3. **Pick an approach** — State assumptions in your head. Only for `estimatedEffort: "high"` or `modifiesBehavior: true` tasks, name the one alternative you rejected and why.

After writing code, the scoped quality gate is your critic — run it (Quality Checks § Per-iteration). Don't add a separate self-critique step; the linters, type-checker, and targeted tests catch more than a re-read does.

---

## Priority Philosophy

In order: **PLAN** (anticipate edge cases) → **PHASE 2 FOUNDATION** (~1 day now to save ~2+ weeks later — take it, we're pre-launch) → **FUNCTIONING CODE** (pragmatic, reliable) → **CORRECTNESS** (compiles, type-checks, scoped tests pass deterministically) → **CODE QUALITY** (clean, no warnings) → **POLISH** (docs, formatting).

Non-negotiables: tests drive implementation; satisfy every `qualityDimensions` entry; handle `Option`/`Result` explicitly (no `unwrap()` in production). For `estimatedEffort: "high"` or `modifiesBehavior: true` tasks, note the one alternative you rejected and why. For everything else, pick and go.

**Prohibited outcomes:**

- Tests that only assert 'no crash' or check type without verifying content
- Tests that mirror implementation internals (break when refactoring)
- A catch-WouldBlock-and-retry loop that re-writes the full buffer (write_all's partial-write semantics duplicate the already-flushed prefix) or busy-spins
- Swallowing genuine write errors (not BrokenPipe/WouldBlock) silently — they must still exit(1) with a message
- Changing the `()` return signatures of output_result/output_json (would ripple to 59 call sites)

---

## Global Acceptance Criteria

These apply to **every** implementation task — the task-level `acceptanceCriteria` returned by `task-mgr next` are layered on top. If any of these fails, the task is not done.

- Rust: No warnings in `cargo check` output
- Rust: No warnings in `cargo clippy -- -D warnings` output
- Rust: All scoped tests pass with `cargo test`
- Rust: `cargo fmt --check` passes
- No breaking changes to existing public APIs — `output_result`/`output_json` keep their `()` signatures
- Every `unsafe` block carries a `// SAFETY:` comment per the existing `claude.rs` convention

---

## Task Files + CLI (IMPORTANT — context economy)

**Never read or edit `tasks/*.json` directly.** Loading the JSON wastes context and editing corrupts loop-engine state. Everything the agent needs about a task is returned by `task-mgr next`; everything global is already embedded in **this prompt file** — that is the authoritative copy. If something here looks inconsistent with the JSON, trust this file and surface the discrepancy.

### Getting your task prefix

```bash
PREFIX=$(jq -r '.taskPrefix' tasks/stdout-eagain-nonblocking.json)
```

Use `$PREFIX` in every CLI call below so you stay scoped to this task list.

### Commands you'll actually run

| Need                                    | Command                                                                                                                                                                           |
| --------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Pick + claim the next eligible task     | `task-mgr next --prefix $PREFIX --claim`                                                                                                                                          |
| Inspect one task (full acceptance etc.) | `task-mgr show $PREFIX-TASK-ID`                                                                                                                                                   |
| List remaining tasks (debug only)       | `task-mgr list --prefix $PREFIX --status todo`                                                                                                                                    |
| Recall learnings relevant to a task     | `task-mgr recall --for-task $PREFIX-TASK-ID`                                                                                                                                      |
| Add a follow-up task (review spawns)    | `echo '{...}' \| task-mgr add --stdin --depended-on-by REVIEW-001`                                                                                                                |
| Mark status                             | Emit `<task-status>$PREFIX-TASK-ID:done</task-status>` (statuses: `done`, `failed`, `skipped`, `irrelevant`, `blocked`)                                                            |

### Files you DO touch

| File                                          | Purpose                                                                |
| --------------------------------------------- | ---------------------------------------------------------------------- |
| `tasks/stdout-eagain-nonblocking-prompt.md`   | This prompt file (read-only)                                           |
| `tasks/progress-$PREFIX.txt`                  | Progress log — **tail** for recent context, **append** after each task |

**Reading progress** — never Read the whole log:

```bash
# Most recent section only
tac tasks/progress-$PREFIX.txt 2>/dev/null | awk '/^---$/{exit} {print}' | tac
```

Skip the read entirely on the first iteration (file won't exist).

---

## Your Task (every iteration)

1. **Resolve prefix and claim the next task**:
   ```bash
   PREFIX=$(jq -r '.taskPrefix' tasks/stdout-eagain-nonblocking.json)
   task-mgr next --prefix $PREFIX --claim
   ```
   The output includes everything you need. If it reports no eligible task, output `<promise>BLOCKED</promise>` with the printed reason and stop.

2. **Pull only the progress context you need** — most iterations want just the most recent section. Skip on the first iteration.

3. **Recall focused learnings** — `task-mgr recall --for-task <TASK-ID>`. Do NOT Read `tasks/long-term-learnings.md` / `tasks/learnings.md` directly.

4. **Verify branch** — `git branch --show-current` matches the printed `branchName` (`fix/stdout-eagain-nonblocking`). Switch if wrong.

5. **Think before coding** — state assumptions; for each `edgeCases`/`failureModes` entry note how it's handled. FEAT-001 is `modifiesBehavior: true` — name the one alternative you rejected (e.g. catch-WouldBlock-and-retry) and why.

6. **Implement** — single task, code and tests in one coherent change.

7. **Run the scoped quality gate** (Quality Checks below — scoped tests only).

8. **Commit**: `fix: <TASK-ID>-completed - [Title]`.

9. **Emit status**: `<task-status><TASK-ID>:done</task-status>`. Do NOT edit the JSON.

10. **Append progress** — ONE tight block, terminated with `---`.

---

## Behavior Modification Protocol (FEAT-001 is `modifiesBehavior: true`)

FEAT-001 changes the *error-path* behavior of `output_result`, `output_migrate_result`, and `output_json` (panic → blocking write / swallow BrokenPipe / exit(1) on genuine error). The **happy path is unchanged** — same bytes to stdout. The callers (59 sites in `src/main.rs`) call these as `()`-returning functions; **do not change the signatures**, so callers are unaffected. No splitting needed — just preserve the `()` contract.

---

## Quality Checks

### Per-iteration scoped gate (FEAT-001)

```bash
cargo fmt --check 2>&1 | tee /tmp/tm-fmt.txt | tail -3
cargo check 2>&1 | tee /tmp/tm-check.txt | tail -3 && grep -i "error\|warning" /tmp/tm-check.txt | head
cargo clippy -- -D warnings 2>&1 | tee /tmp/tm-clippy.txt | tail -3 && grep "^error\|^warning" /tmp/tm-clippy.txt | head
cargo test handlers 2>&1 | tee /tmp/tm-test.txt | tail -5 && grep -i "FAILED\|error\[" /tmp/tm-test.txt | head
```

Per CLAUDE.md: pipe to a temp file via `tee` and grep results in the **same** command — never stream full output, never run twice. **Do NOT** run the entire workspace suite during FEAT-001 — that's REVIEW-001's job.

### Full gate (REVIEW-001 only)

```bash
cargo fmt --check && cargo check && cargo clippy -- -D warnings && cargo test
```

REVIEW-001 fixes every failure including pre-existing. Below ~12 unrelated failures, just fix them; above that and clearly unrelated, spawn a single FIX-xxx + `<promise>BLOCKED</promise>`.

---

## Common Wiring Failures (REVIEW-001 reference)

- A bare `print!`/`println!` of large report text left un-routed through `write_stdout` → grep `output_result` / `output_migrate_result` / `output_json` to confirm all three sites converted.
- `unsafe` fcntl block missing a `// SAFETY:` comment, or bit math that *sets* O_NONBLOCK instead of clearing it.
- Test that wraps a raw fd in `File` via `from_raw_fd` AND also `libc::close`es it → double-close/EBADF. Let the `File` own the write end; close only the read end manually.

---

## Review Tasks

REVIEW-001 (priority 99, model grok-4, 1800s) runs the full unscoped suite, verifies the unsafe correctness and the three converted sites, and spawns `FIX-xxx` (priority 50-97) for any issue via:

```sh
echo '{
  "id": "FIX-001",
  "title": "Fix: <specific issue>",
  "description": "From REVIEW-001: <details>",
  "rootCause": "<file:line + issue>",
  "exactFix": "<specific change>",
  "verifyCommand": "<shell command that proves the fix>",
  "acceptanceCriteria": ["Issue resolved", "No new warnings"],
  "priority": 60,
  "touchesFiles": ["src/handlers.rs"]
}' | task-mgr add --stdin --depended-on-by REVIEW-001
```

If no issues, emit `<task-status>REVIEW-001:done</task-status>` with a one-line "Clean review" note. Use the **rust-python-code-reviewer** agent for the review pass.

---

## Progress Report Format

APPEND to `tasks/progress-$PREFIX.txt` (create with a one-line header if missing). Keep it tight (~10 lines):

```
## [YYYY-MM-DD HH:MM] - [TASK-ID]
Approach: [one sentence — what you chose and why]
Files: [comma-separated paths touched]
Learnings: [1-3 bullets, one line each]
---
```

---

## Stop and Blocked Conditions

### Stop Condition

Before `<promise>COMPLETE</promise>`: verify ALL tasks `passes: true`, no new tasks created in final review, REVIEW-001 passed with full suite green.

```
<promise>COMPLETE</promise>
```

### Blocked Condition

Document the blocker in the progress file, create a clarification task via `task-mgr add --stdin --depended-on-by <blocked-task>` (priority 0), then:

```
<promise>BLOCKED</promise>
```

---

## Reference Code

**Existing libc + raw-fd style to mirror** — `src/loop_engine/claude.rs::open_pty_for_child_output` (~:94-141): uses `libc::openpty`, `libc::tcgetattr`/`tcsetattr`, `std::os::fd::{OwnedFd, FromRawFd, AsRawFd}`, with `// SAFETY:` comments on each `unsafe` block. Follow this exact convention for the new `fcntl` calls.

**The three sites to convert** in `src/handlers.rs`:

```rust
// :145-154  output_result
pub fn output_result<T: serde::Serialize + TextFormattable>(result: &T, format: OutputFormat) {
    match format {
        OutputFormat::Json => { output_json(result); }
        OutputFormat::Text => { print!("{}", result.format_text()); }   // -> write_stdout(&result.format_text())
    }
}

// :160-166  output_migrate_result (text arm)
//   print!("{}", format_migrate_text(result, action));                // -> write_stdout(&format_migrate_text(result, action))

// :198-206  output_json
pub fn output_json<T: serde::Serialize>(result: &T) {
    match serde_json::to_string_pretty(result) {
        Ok(json) => println!("{}", json),                              // -> write_stdout(&format!("{}\n", json))
        Err(e) => { eprintln!("Error: failed to serialize result to JSON: {}", e); process::exit(1); }  // UNCHANGED
    }
}
```

**Target helper shape** (all new, in `src/handlers.rs`):

```rust
/// Clear O_NONBLOCK on `fd` if set. A child (Node/libuv) can flip our stdout's
/// open file description to non-blocking via an inherited+aliased fd (`2>&1`
/// makes fd 1 and fd 2 share one OFD); a later large write then EAGAINs.
fn clear_nonblocking(fd: libc::c_int) {
    // SAFETY: F_GETFL / F_SETFL on a valid fd take no pointers; we only clear a flag bit.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags >= 0 && (flags & libc::O_NONBLOCK) != 0 {
            let _ = libc::fcntl(fd, libc::F_SETFL, flags & !libc::O_NONBLOCK);
        }
    }
}

/// Write `bytes` to `w` (backed by `fd`) without panicking. Clears O_NONBLOCK
/// first so the write blocks instead of EAGAINing; swallows BrokenPipe; exits(1)
/// on other errors.
///
/// Race note: race-free for a final print after children are joined (the dedup
/// case). Under concurrent live children a libuv child can re-flip the flag after
/// the clear — the `Err(_) => exit(1)` arm is the backstop, so we never unwind,
/// but "always prints in full" is best-effort, not absolute, under concurrency.
fn write_all_blocking<W: std::io::Write>(fd: libc::c_int, w: &mut W, bytes: &[u8]) {
    clear_nonblocking(fd);
    match w.write_all(bytes).and_then(|_| w.flush()) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {}
        Err(e) => {
            eprintln!("Error: failed to write to stdout: {}", e);
            process::exit(1);
        }
    }
}

fn write_stdout(s: &str) {
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    write_all_blocking(libc::STDOUT_FILENO, &mut lock, s.as_bytes());
}
```

**Test pattern** (`#[cfg(test)]` in `src/handlers.rs`) — drive `write_all_blocking` over a real pipe via the `fd` seam:

- Create a pipe with `libc::pipe(&mut fds)`; set the write end O_NONBLOCK via `fcntl(F_SETFL, flags | O_NONBLOCK)`.
- **clear test**: call `clear_nonblocking(write_fd)`, assert `fcntl(F_GETFL) & O_NONBLOCK == 0`.
- **negative control**: with the pipe full and no reader, a raw `File::from_raw_fd(write_fd).write_all(&payload_128k)` returns `ErrorKind::WouldBlock` (proves the bug).
- **fix test**: spawn a reader thread draining the read end into a `Vec`; wrap **only** the write end in `File` via `from_raw_fd` (it owns/closes it); write 128 KB via `write_all_blocking`; join reader; assert all bytes received. Close the read end from the side the thread doesn't own — avoid double-close.
- **BrokenPipe test**: close the read end first, then `write_all_blocking` on the write end → returns normally.

---

## Key Learnings (from task-mgr recall)

Treat these as authoritative — do NOT Read the learnings files unless a task needs one not here.

- **[3068] / [693]** `.expect("invariant")` / documented IO assumptions are house style in `runner.rs`/`claude.rs` — but for THIS change prefer explicit `Result` handling, since the entire goal is to *not* panic on a write. Reserve `unsafe` for the fcntl FFI only.
- **[3129]** Write commands emit context headers to **stderr** first; stdout carries data only. This change touches stdout only — the stderr path (`emit_prefixed_lines`) already swallows write errors and is explicitly out of scope.
- **[3813]** Output result types register via `impl_text_formattable!`; `format_text()` returns a `String`. Write it as-is — no per-type print logic changes.
- **[3680]** Verify quality gates by piping to a temp file and grepping in the **same** command (see Quality Checks). Never stream full output through the tool; never run a command twice to see tail then grep.

---

## CLAUDE.md Excerpts (only what applies to this change)

- **Test/build output**: ALWAYS pipe test runners/linters/builds to a temp file via `tee`, then grep for results **in the same command**. Never stream full output; never run once for tail then again to grep. Example: `cargo test 2>&1 | tee /tmp/test.txt | tail -3 && grep "FAILED\|error\[" /tmp/test.txt | head`.
- **Core principles**: Testability (DI mandatory — hence the fd-parameterized `write_all_blocking`); Robustness (validate at boundaries, fail fast); Reliable (write a test that recreates the bug, then fix it — hence the negative-control WouldBlock test).
- **Surgical changes**: change only what the task requires; the fix is confined to `src/handlers.rs`, no adjacent cleanup, no new abstractions with one use.
- **Never edit `tasks/*.json` directly** — use the CLI + `<task-status>` tags.

---

## Important Rules

- Work on **ONE task per iteration**
- **Commit frequently** after each passing task
- **Keep CI green** — never commit failing code
- **Read before writing** — always read files first
- **Minimal changes** — only implement what's required
- Work on the correct branch: **fix/stdout-eagain-nonblocking**
