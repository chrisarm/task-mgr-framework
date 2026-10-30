# Shape — worktree-root (final, revised)

Source: first-principles-architect, 2026-10-22 (v3; supersedes v1 + addendum). Read-only; nothing built or run.
Operator inputs folded: two heavy repos (restaurant_agent, mw_support); Dropbox client not running; 291 repos / ~95 duplicated basenames.

## 1. Reduced shape (pin candidates)

1. **Config key.** Optional `worktreeRoot` (absolute path) in user config `~/.config/task-mgr/config.json`, read by `UserConfig` (src/loop_engine/user_config.rs:24). Absent → paths byte-identical to today (`{parent}/{repo}-worktrees/{branch}`); existing tests at worktree.rs:2640-2760 and 3668-3690 still pass.
2. **Layout.** Key set → new worktrees at `<root>/<repo-name>-<repo-key>/<sanitized-branch>`; slots at `.../<branch>-slot-N`. `repo-name` = dir name of `git::main_repo_root_at(project_root)` (src/git/mod.rs:41), not `project_root.file_name()`. `repo-key` = first 8 lowercase hex of md5 of the canonicalized absolute path from `git rev-parse --path-format=absolute --git-common-dir`, as raw OS bytes. `md5` crate already a dependency (Cargo.toml:48); std `DefaultHasher` must not be used.
3. **Same result from main checkout or worktree.** The common dir is shared, so `repo-name-key` is identical whether `project_root` is the main checkout, slot 0, or a slot worktree. Tested explicitly.
4. **Resolve once, inject.** Root and repo identity (`name`, `key`) resolved once per loop/command and passed into `compute_worktree_path`, which stays pure (no git calls). All 6 production call sites (worktree.rs:222, 589/591, 934, 1113, 2548).
5. **Containment check.** Release-build `assert!` (worktree.rs:102) checks against `<root>/<repo-name>-<repo-key>`.
6. **Invalid root fails fast.** Set but not absolute, missing, not a directory, not writable, or equal to/inside the main repo → error before creating anything, naming the config key. Never `create_dir_all` the root (a missing dataset would silently become a plain dir on snapshotted /home).
7. **Ownership check backstop.** Before reusing an existing dir under the per-repo dir, compare its git common dir with ours; error on mismatch (existing reuse check only compares branch, worktree.rs:257-266).
8. **Cleanup scope.** Empty-parent cleanup (worktree.rs:471-474) may remove `<root>/<repo-name>-<repo-key>/`; never the root itself or anything outside it.
9. **Old worktrees keep working.** Found via `git worktree list` by branch (worktree.rs:242-253). Test only.
10. **Two-repo integration test.** Two same-named repos share one root → separate dirs; removing one repo's last worktree leaves the other's dir and the root intact.
11. **Non-sibling integration test.** Worktree under a non-sibling root; `git -C <wt> rev-parse --git-common-dir` points back to the main repo.
12. **Docs.** README.md:314, CLAUDE.md:7, src/loop_engine/CLAUDE.md:1042, docs/INTEGRATION.md §Git Worktrees.

## 2. Dropped / deferred (restoration path)

- Env-var override — defer. Later: env > config precedence in the resolver.
- Per-project override — drop. Later: `ProjectConfig` field (project_config.rs:702) taking precedence.
- `~` expansion — defer; require absolute. Later: expand leading `~/`.
- Migration / move command — drop (owner does it).
- Per-language build redirection (`_build`, `deps`, `node_modules`, `.venv`) — drop; under the root they are un-snapshotted automatically, incl. rustler `_build/<env>/native/`. Later: env injection (`MIX_BUILD_PATH`, `MIX_DEPS_PATH`) or a setup hook.
- Sharing `deps/`/`_build` between worktrees — defer (rebuild speed, not retention). Later: measure cold build time, then `cp --reflink` seeding.
- Changing the root while a loop runs — documented unsupported (risk 2).

## 3. Phases

One phase: repo-identity resolver, signature change, validation, guards, tests, docs.

## 4. Risks / failure conditions

1. Name collisions routine → pins 2, 7, 10. 8 hex over ~291 repos ≈ 1e-5 hash-collision chance; ownership check makes any collision a clear error.
2. Root or repo moved between creating and cleaning up slots: slots 1+ recompute paths (worktree.rs:934, 1113, 2548); `cleanup_slot_worktrees` treats path as gone (2549-2553); `git branch -D` refuses → safe but orphaned worktree+branch. Moving the main repo changes `repo-key` (git needs `git worktree repair` anyway). Documented limitations.
3. Running from inside a worktree: pins 2/3 fix the `project_root.file_name()` quirk (worktree.rs:86) in configured mode only; default layout unchanged.
4. Root missing/relative/unwritable/inside repo → pin 6.
5. No sibling assumptions found: task-mgr src/ has no copy/bootstrap of build dirs or sibling-relative joins; restaurant_agent loads NIFs by crate name (lib/restaurant_agent/native/embeddings.ex:11, audio.ex:14), no `path = "../..."` in native/*/Cargo.toml, `HTTP_PORT` per worktree via env (config/runtime.exs:14); mw_support has no relative path deps, no target-dir override. Not inspected: mix.lock, scripts outside mix.exs/config/native.
6. Owner migration caveat: worktrees moved with `git worktree move` may hold absolute paths in `_build`/`.venv` shebangs → rebuild (`mix deps.get && mix compile --force`, recreate `.venv`). New worktrees unaffected.

## 5. Recommendation

- Mechanism: user-config key only (per-machine, all repos; one source, no precedence bugs).
- Layout: `<root>/<repo-name>-<8hex md5(canonical git-common-dir)>/<branch>`.

## Open question

- Missing root: hard error or warn + fall back to default layout? Reviewer recommends hard error.
