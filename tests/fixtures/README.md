# Test Fixtures

Test PRDs and mock Claude scripts for testing the loop engine.

## Files

| File | Purpose |
|------|---------|
| `smoke-test-prd.json` | Minimal 3-task PRD for smoke testing. Serial dependency chain (SMOKE-001 -> 002 -> 003) with synergy and file overlap relationships. |
| `mock-claude-smoke.sh` | Mock Claude binary for smoke tests. Extracts task ID from prompt, optionally marks it complete via task-mgr, outputs `<promise>COMPLETE</promise>`. |
| `test-loop-prd.json` | 3-task PRD used by E2E integration tests in `e2e_loop.rs`. |
| `mock-claude.sh` | Mock Claude binary for E2E tests. Requires `TASK_MGR_BIN`, `TASK_MGR_DIR`, and `MOCK_RUN_ID` env vars. |
| `sample_prd.json` | 7-task PRD with various relationships (dependencies, synergy, batch, conflicts). Used by task selection tests. |
| `*.json.tmpl`     | Templated PRD fixtures. `{{OPUS_MODEL}}` / `{{SONNET_MODEL}}` / `{{HAIKU_MODEL}}` are substituted at test time by `tests/common/mod.rs::render_fixture_tmpl` using the constants in `src/loop_engine/model.rs`, so model bumps only touch one file. To add a new template, drop `foo.json.tmpl` here and call `render_fixture_tmpl("foo.json", temp_dir)` from the test. |

## Smoke Test Usage

### Manual smoke test with task-mgr loop

```bash
# Build task-mgr
cargo build --release

# Run the loop with the smoke test PRD and mock Claude
CLAUDE_BINARY=./tests/fixtures/mock-claude-smoke.sh \
  cargo run -- loop tests/fixtures/smoke-test-prd.json -y
```

### With explicit prompt file

```bash
# Create a minimal prompt file
echo "Implement the task described below." > /tmp/smoke-prompt.md

CLAUDE_BINARY=./tests/fixtures/mock-claude-smoke.sh \
  cargo run -- loop tests/fixtures/smoke-test-prd.json \
    --prompt-file /tmp/smoke-prompt.md -y
```

### Environment variables

| Variable | Required | Description |
|----------|----------|-------------|
| `CLAUDE_BINARY` | Yes | Path to the mock Claude script (overrides default `claude` binary) |
| `TASK_MGR_BIN` | No | Path to task-mgr binary (mock script uses it to mark tasks complete) |
| `TASK_MGR_DIR` | No | Project directory for task-mgr `--dir` flag |
| `MOCK_RUN_ID` | No | Run ID passed to `task-mgr complete --run-id` |
