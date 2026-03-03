#!/bin/bash
# Mock Claude binary that outputs stream-json format lines.
# Used by integration tests to verify spawn_claude parses stream-json correctly.
#
# Outputs: system init, assistant message (text + tool_use), user message (tool_result),
# and a final result line.

cat <<'EOF'
{"type":"system","subtype":"init","session_id":"test-session","data":{}}
{"type":"assistant","message":{"content":[{"type":"text","text":"Let me read the file."},{"type":"tool_use","id":"toolu_abc123","name":"Read","input":{"file_path":"/src/main.rs"}}]},"model":"claude-sonnet-4-6","error":null}
{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_abc123","content":"fn main() { println!(\"hello\"); }","is_error":false}]}}
{"type":"assistant","message":{"content":[{"type":"text","text":"The file contains a main function."}]},"model":"claude-sonnet-4-6","error":null}
{"type":"result","subtype":"success","result":"<completed>TASK-001</completed>","session_id":"test-session"}
EOF
