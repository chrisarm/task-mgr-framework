#!/bin/bash
# Deterministic capture harness for the engine.rs carve dogfood gate.
#
# Produces a normalized stderr transcript and a normalized DB-final-state dump
# for one loop scenario, in an isolated, FIXED-path working tree (so worktree
# and DB paths are reproducible across runs). REVIEW-001 re-runs this on the
# post-carve branch and diffs the output against the frozen baselines committed
# alongside this script.
#
# Usage:
#   capture.sh <scenario> <parallel> <out_stderr> <out_db>
#     scenario : basename of scenarios/<scenario>.json (e.g. "sequential", "wave")
#     parallel : 1 = sequential run_iteration; 2 = 2-slot wave
#     out_stderr / out_db : destination files for the normalized captures
#
# Determinism strategy:
#   * Fixed working root under $WORK_BASE (rm -rf'd each run) → stable paths.
#   * mock-runner.sh emits a fixed <completed> tag → no LLM, no PRNG, no clock.
#   * Usage check + auto-review disabled (network + Claude subprocess, both
#     non-deterministic and slow).
#   * normalize() collapses every remaining non-deterministic field (wall-clock
#     timestamps, ISO datetimes, durations, run IDs, the fixed work path, the
#     git commit SHAs the loop scans) to stable sentinels.
#
# What is byte-identical vs set-equal: see README.md. The DB dump is sorted and
# byte-identical. Sequential stderr is line-ordered and byte-identical; wave
# stderr is only set-equal after normalization (slot interleave is timing
# dependent) — compare it with `sort`.

set -euo pipefail

SCENARIO="${1:?scenario}"
PARALLEL="${2:?parallel}"
OUT_STDERR="${3:?out_stderr}"
OUT_DB="${4:?out_db}"

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$HERE/../../.." && pwd)"
SCEN_DIR="$HERE/scenarios"
MOCK="$HERE/mock-runner.sh"

# Resolve the task-mgr binary (cargo target dir may be relocated).
BIN="${TASK_MGR_BIN:-}"
if [[ -z "$BIN" ]]; then
    TARGET_DIR="$(cd "$REPO_ROOT" && cargo metadata --no-deps --format-version 1 2>/dev/null \
        | tr ',' '\n' | grep target_directory | grep -oP '"\K[^"]+(?=")' | tail -1)"
    BIN="${TARGET_DIR:-$REPO_ROOT/target}/debug/task-mgr"
fi
[[ -x "$BIN" ]] || { echo "capture: task-mgr binary not found at $BIN (run: cargo build --bin task-mgr)" >&2; exit 1; }

WORK_BASE="${WORK_BASE:-/tmp/carve-baseline}"
WORK="$WORK_BASE/$SCENARIO/repo"
rm -rf "$WORK_BASE/$SCENARIO"
mkdir -p "$WORK/tasks"

# --- isolated git repo (deterministic identity) -----------------------------
git -C "$WORK" init -q
git -C "$WORK" config user.email "baseline@carve.test"
git -C "$WORK" config user.name "Carve Baseline"
git -C "$WORK" config commit.gpgsign false
: > "$WORK/.gitkeep"
git -C "$WORK" add -A
git -C "$WORK" commit -q -m "initial"

BRANCH="$(grep -oP '"branchName"\s*:\s*"\K[^"]+' "$SCEN_DIR/$SCENARIO.json")"
# Sequential runs in --no-worktree mode → the loop checks out the PRD branch in
# place, so create it. Wave runs in worktree mode → leave the repo on its
# initial branch so slot 0 gets its OWN worktree (the real wave topology that
# defense layer #1 threads the slot-0 path through); checking the branch out
# here would alias slot 0 to the main working tree.
if [[ "$PARALLEL" -le 1 ]]; then
    git -C "$WORK" checkout -q -b "$BRANCH"
fi

cp "$SCEN_DIR/$SCENARIO.json" "$WORK/tasks/$SCENARIO.json"
cp "$SCEN_DIR/$SCENARIO-prompt.md" "$WORK/tasks/$SCENARIO-prompt.md"

# --- run the loop, capturing raw stderr -------------------------------------
RAW_STDERR="$(mktemp)"
WORKTREE_FLAG=(--no-worktree)
[[ "$PARALLEL" -gt 1 ]] && WORKTREE_FLAG=(--cleanup-worktree)

env -C "$WORK" \
    TASK_MGR_DIR="$WORK/.task-mgr" \
    TASK_MGR_BIN="$BIN" \
    CLAUDE_BINARY="$MOCK" \
    LOOP_USAGE_CHECK_ENABLED=false \
    TASK_MGR_NO_EXTRACT_LEARNINGS=1 \
    "$BIN" loop init "tasks/$SCENARIO.json" >/dev/null 2>>"$RAW_STDERR" || true

env -C "$WORK" \
    TASK_MGR_DIR="$WORK/.task-mgr" \
    TASK_MGR_BIN="$BIN" \
    CLAUDE_BINARY="$MOCK" \
    LOOP_USAGE_CHECK_ENABLED=false \
    TASK_MGR_NO_EXTRACT_LEARNINGS=1 \
    "$BIN" loop run "tasks/$SCENARIO.json" \
        --yes --no-auto-review --parallel "$PARALLEL" "${WORKTREE_FLAG[@]}" \
        >/dev/null 2>>"$RAW_STDERR" || true

# --- normalize stderr -------------------------------------------------------
# Collapse every non-deterministic field to a stable sentinel.
normalize_stderr() {
    sed -E \
        -e "s#$WORK_BASE/[^ '\"]*#<WORK>#g" \
        -e 's/[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9:.+-]+/<TIMESTAMP>/g' \
        -e 's/[0-9]{4}-[0-9]{2}-[0-9]{2} [0-9]{2}:[0-9]{2}:[0-9]{2}/<TIMESTAMP>/g' \
        -e 's/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/<UUID>/g' \
        -e 's/run_[0-9a-f]{6,}/run_<ID>/g' \
        -e 's/\b[0-9a-f]{40}\b/<SHA>/g' \
        -e 's/\b[0-9a-f]{7,12}\b/<SHA>/g' \
        -e 's/([0-9]+m )?[0-9]+(\.[0-9]+)?s\b/<DURATION>/g' \
        -e 's/[0-9]+(\.[0-9]+)?(ms|µs|us|ns)\b/<DURATION>/g' \
        -e 's/\bpid[: ]+[0-9]+/pid <PID>/gI'
}
# Sequential stderr is line-ordered and byte-identical run to run. Wave stderr
# interleaves per-slot lines in timing-dependent order, so its contract is
# set-equality after normalization — emit it SORTED so the frozen baseline and
# any re-capture are directly `diff`-able (no separate sort step at the gate).
# See README.md "Wave-mode stderr ordering".
if [[ "$PARALLEL" -gt 1 ]]; then
    normalize_stderr < "$RAW_STDERR" | sort > "$OUT_STDERR"
else
    normalize_stderr < "$RAW_STDERR" > "$OUT_STDERR"
fi

# --- normalize DB-final-state ----------------------------------------------
# Single deterministic command (see AC). Sorted so row insertion order does not
# affect byte-identity; timestamps / run IDs / paths collapsed to sentinels.
sqlite3 "$WORK/.task-mgr/tasks.db" .dump \
    | sed -E \
        -e 's/[0-9]{4}-[0-9]{2}-[0-9]{2}[T ][0-9:.+-]+/TIMESTAMP/g' \
        -e 's/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/<UUID>/g' \
        -e 's/run_[0-9a-f]{6,}/run_ID/g' \
        -e "s#$WORK_BASE/[^ '\"]*#<WORK>#g" \
        -e 's/\b[0-9a-f]{40}\b/<SHA>/g' \
    | sort > "$OUT_DB"

rm -f "$RAW_STDERR"
echo "captured: $SCENARIO -> $OUT_STDERR ($(wc -l < "$OUT_STDERR") lines), $OUT_DB ($(wc -l < "$OUT_DB") lines)" >&2
