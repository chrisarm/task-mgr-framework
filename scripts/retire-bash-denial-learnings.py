#!/usr/bin/env python3
"""Retire learnings about Bash permission denials from task-mgr databases.

These learnings were artifacts of running Claude Loop in don't-ask mode
with restricted Bash permissions — a transient configuration issue, not
a persistent pattern worth remembering.

Usage:
    # Dry run (default) — show what would be retired
    python scripts/retire-bash-denial-learnings.py /path/to/.task-mgr/tasks.db

    # Actually retire them
    python scripts/retire-bash-denial-learnings.py /path/to/.task-mgr/tasks.db --apply

    # Scan multiple databases
    python scripts/retire-bash-denial-learnings.py db1/tasks.db db2/tasks.db --apply
"""

from __future__ import annotations

import argparse
import sqlite3
import sys
from datetime import datetime, timezone
from pathlib import Path

# Patterns that identify bash-denial learnings (matched against title + content)
SEARCH_PATTERNS = [
    "bash denied",
    "bash is denied",
    "bash tool denied",
    "bash permission",
    "bash tool%permission",
    "permission-blocked",
    "permission denied",
    "don't-ask mode",
    "dont-ask mode",
    "don't-ask permission",
]

# Patterns that should NOT be retired even if they match above
# (e.g., legitimate permission/security learnings)
EXCLUDE_PATTERNS = [
    "RBAC",
    "authorization",
    "file permission",
    "chmod",
]


def find_bash_denial_learnings(db_path: Path) -> list[dict]:
    """Find active learnings related to bash denial/permissions."""
    conn = sqlite3.connect(db_path)
    conn.row_factory = sqlite3.Row
    try:
        # Build parameterized WHERE clause
        params: list[str] = []
        search_conds = []
        for pat in SEARCH_PATTERNS:
            like_pat = f"%{pat}%"
            search_conds.append("(title LIKE ? OR content LIKE ?)")
            params.extend([like_pat, like_pat])

        exclude_conds = []
        for pat in EXCLUDE_PATTERNS:
            exclude_conds.append("title NOT LIKE ?")
            params.append(f"%{pat}%")

        where_search = " OR ".join(search_conds)
        where_exclude = " AND ".join(exclude_conds)

        query = f"""
            SELECT id, outcome, confidence, title, substr(content, 1, 300) as preview,
                   retired_at
            FROM learnings
            WHERE ({where_search})
              AND {where_exclude}
              AND retired_at IS NULL
            ORDER BY id
        """
        return [dict(row) for row in conn.execute(query, params).fetchall()]
    finally:
        conn.close()


def retire_learnings(db_path: Path, learning_ids: list[int]) -> int:
    """Set confidence=low and retired_at=now for the given learnings."""
    if not learning_ids:
        return 0
    conn = sqlite3.connect(db_path)
    try:
        now = datetime.now(timezone.utc).strftime("%Y-%m-%d %H:%M:%S")
        placeholders = ",".join("?" for _ in learning_ids)
        conn.execute(
            f"""
            UPDATE learnings
            SET confidence = 'low', retired_at = ?
            WHERE id IN ({placeholders}) AND retired_at IS NULL
            """,
            [now, *learning_ids],
        )
        affected = conn.total_changes
        conn.commit()
        return affected
    finally:
        conn.close()


def process_db(db_path: Path, *, apply: bool) -> int:
    """Process a single database. Returns count of learnings found."""
    if not db_path.exists():
        print(f"  SKIP: {db_path} does not exist")
        return 0

    matches = find_bash_denial_learnings(db_path)
    if not matches:
        print(f"  {db_path}: no bash-denial learnings found")
        return 0

    print(f"  {db_path}: found {len(matches)} bash-denial learning(s)")
    for m in matches:
        status = "RETIRED" if apply else "WOULD RETIRE"
        print(f"    [{status}] #{m['id']} ({m['confidence']}) {m['title']}")

    if apply:
        ids = [m["id"] for m in matches]
        retired = retire_learnings(db_path, ids)
        print(f"    -> Retired {retired} learning(s)")

    return len(matches)


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Retire bash-denial learnings from task-mgr databases"
    )
    parser.add_argument(
        "databases",
        nargs="+",
        type=Path,
        help="Path(s) to .task-mgr/tasks.db files",
    )
    parser.add_argument(
        "--apply",
        action="store_true",
        help="Actually retire learnings (default is dry-run)",
    )
    args = parser.parse_args()

    if not args.apply:
        print("DRY RUN (pass --apply to actually retire)\n")

    total = 0
    for db_path in args.databases:
        total += process_db(db_path, apply=args.apply)

    print(f"\nTotal: {total} bash-denial learning(s) {'retired' if args.apply else 'found'}")
    if not args.apply and total > 0:
        print("Re-run with --apply to retire them.")

    sys.exit(0 if total == 0 or args.apply else 1)


if __name__ == "__main__":
    main()
