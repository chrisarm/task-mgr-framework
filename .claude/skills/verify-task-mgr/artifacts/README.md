# Verification artifacts

Proof from a `verify-task-mgr` run lands in a subdirectory named with the sandbox run id (`<run-id>/`).

Each `$H capture NAME` writes `NAME.cmd.txt`, `NAME.stdout.txt`, `NAME.stderr.txt`, and `NAME.exit.txt`. `$H snapshot-db NAME` writes `NAME.db.txt`.

Cleanup deletes the `/tmp/task-mgr-verify-<run-id>/` sandbox and must leave this directory in place.
