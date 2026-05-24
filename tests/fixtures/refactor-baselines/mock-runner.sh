#!/bin/bash
# Deterministic mock Claude runner for the engine.rs carve dogfood gate.
#
# Unlike tests/fixtures/mock-claude.sh (which emits <promise>COMPLETE</promise>),
# this mock emits a proper `<completed>TASK-ID</completed>` tag so the loop
# engine's OWN completion counters (agg.tasks_completed, slot result handling)
# register the completion — that is the observable behavior the dogfood gate
# diffs. It does not touch the DB itself; the loop's completion detection +
# TaskLifecycle own the status write.
#
# Invoked as: mock-runner.sh [flags...] --session-id <uuid> -p
# The real Claude CLI takes the prompt on STDIN (the trailing `-p` flag carries
# no value); all positional flags are ignored here.

set -euo pipefail

# Drain the prompt from stdin (the loop pipes it). Fall back to args for manual
# invocation convenience, but stdin is the production shape.
PROMPT="$(cat || true)"

if [[ -z "$PROMPT" ]]; then
    echo "mock-runner: empty prompt on stdin" >&2
    exit 1
fi

# The loop injects the task under work as a JSON block whose first "id" field is
# the active task. Extract it the same way the other mocks do.
TASK_ID=$(printf '%s' "$PROMPT" | grep -oP '"id"\s*:\s*"\K[^"]+' | head -1)

if [[ -z "$TASK_ID" ]]; then
    echo "mock-runner: could not extract task id from prompt" >&2
    exit 1
fi

# Emit Claude's stream-json (JSONL) shape — the loop runs the binary with
# `--verbose --output-format stream-json` and parses each line as JSON. The
# final `result` line's `.result` field is where completion detection looks for
# the `<completed>` tag. Fixed session_id keeps output deterministic (the random
# --session-id the loop passes on argv is ignored here and never reaches stderr
# or the DB).
printf '%s\n' '{"type":"system","subtype":"init","session_id":"mock-session","data":{}}'
printf '{"type":"assistant","message":{"content":[{"type":"text","text":"MOCK completing %s"}]},"model":"claude-opus-4-7","error":null}\n' "$TASK_ID"
printf '{"type":"result","subtype":"success","result":"<completed>%s</completed>","session_id":"mock-session"}\n' "$TASK_ID"
exit 0
