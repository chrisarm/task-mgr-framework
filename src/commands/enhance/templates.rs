//! Static template strings rendered into CLAUDE.md / AGENTS.md by
//! `task-mgr enhance agents`.
//!
//! Templates are plain `&'static str` for v1 — no format-string interpolation
//! is performed. Project-specific interpolation is deferred to a future PR.
//!
//! Two profiles are exposed:
//! - [`WORKFLOW_TEMPLATE`] — task-mgr CLI cheat sheet and workflow patterns
//!   (mirrors §3a of the user-global CLAUDE.md, project-scoped).
//! - [`FULL_TEMPLATE`] — `WORKFLOW_TEMPLATE` content plus general LLM-coding
//!   guidelines (Think Before Coding, Simplicity First, Surgical Changes,
//!   Goal-Driven Execution).
//!
//! Invariant: `FULL_TEMPLATE` is by construction the concatenation of
//! `WORKFLOW_TEMPLATE` and the LLM-guidelines literal — both are produced
//! from the same `workflow_body!()` and `llm_guidelines_body!()` macros, so
//! the two profiles cannot drift.

/// Markers that fence the task-mgr-managed block inside the target file.
/// These are the *only* substrings the enhance command will rewrite.
pub const MARKER_BEGIN: &str = "<!-- TASK_MGR:BEGIN -->";
pub const MARKER_END: &str = "<!-- TASK_MGR:END -->";

/// The workflow body literal. Emitted by both `WORKFLOW_TEMPLATE` and
/// `FULL_TEMPLATE` so the two cannot drift.
macro_rules! workflow_body {
    () => {
        r#"## task-mgr workflow

This block is managed by `task-mgr enhance` — edits inside the
`TASK_MGR:BEGIN` / `TASK_MGR:END` markers will be overwritten on the next
`task-mgr enhance agents` run. Content outside the markers is preserved.

### CLI cheat sheet

- **Look up a task**: `task-mgr show <task-id>`
- **List tasks**: `task-mgr list` (filter with `--status`, `--prefix`, `--task-type`)
- **Pick the next eligible task**: `task-mgr next` (add `--claim` to mark it
  `in_progress`)
- **Add a new task** (review fix / refactor / follow-up): pipe a single JSON
  object to `task-mgr add --stdin`. Example:

  ```sh
  echo '{"id":"CODE-FIX-001","title":"Fix race","difficulty":"medium","touchesFiles":["src/foo.rs"]}' \
    | task-mgr add --stdin
  ```

  Priority is auto-computed (top-task priority minus one). Pass
  `--depended-on-by <milestone-id>` when the new task should block a
  milestone — this is the canonical way to wire spawn-fixups into the
  correct PRD (see "Spawn-fixup PRD targeting" below).

- **Mark status from a loop iteration**: emit a `<task-status>` tag in your
  output. Recognized statuses: `done`, `failed`, `skipped`, `irrelevant`,
  `blocked`. Example: `<task-status>TASK-ID:done</task-status>`.

### Mid-loop JSON sync

When the task-list JSON changes mid-effort (adding tasks, editing
descriptions, recording human-review outcomes), NEVER run bare
`task-mgr init --from-json <prd>.json` — it wipes `status`, `started_at`,
and `completed_at` for every task in the list.

Correct incremental sync (post this PR, `task-mgr loop init` is the
canonical form; `task-mgr init --from-json` survives indefinitely as a
deprecated shim that prints a stderr notice and dispatches to the same
path):

```sh
task-mgr loop init <prd>.json --append --update-existing --dry-run  # preview
task-mgr loop init <prd>.json --append --update-existing             # apply
```

`--append --update-existing` preserves status fields on existing rows,
refreshes `description` / `acceptanceCriteria` / `notes` / `files` /
relationships, and adds any new tasks. Safe to run against an
in-progress loop.

### Human-in-the-loop CLARIFY tasks

When a task requires human sign-off (`requires_human: true`), the loop
emits `<promise>BLOCKED</promise>` until resolution. On resolution,
embed a machine-readable `humanReviewOutcome` block directly in the
JSON task entry:

```json
"humanReviewOutcome": {
  "resolvedAt": "YYYY-MM-DD",
  "resolvedBy": "<name>",
  "confirmedValues": { },
  "deltasFromProposed": [ ],
  "additionalRequirements": [ ]
}
```

Then update downstream task entries in the SAME commit — their embedded
rate-limit / threshold / flag values must match the confirmed outcome,
or the loop will implement the proposed (wrong) value. Sync with
`task-mgr loop init <prd>.json --append --update-existing`, then
`task-mgr complete <clarify-task-id>`.

### Spawn-fixup PRD targeting

When a milestone or CODE-REVIEW iteration spawns ad-hoc fixup tasks
(`CODE-FIX-`, `WIRE-FIX-`, `IMPL-FIX-`, `REFACTOR-N-`), the
`task-mgr add --stdin` invocation MUST disambiguate the destination PRD
or the entry leaks into whatever PRD JSON the CLI defaults to. Two
reliable forms:

- (a) `--from-json tasks/<correct-prd>.json` — explicit path
- (b) `--depended-on-by <milestone-of-correct-prd>` — the prefix is
  unambiguous from the dependency edge

Symptom of getting it wrong: orphan `passes: false` placeholders show
up in an unrelated PRD's JSON during merge-back review.

### Learnings, recall, decisions

- `task-mgr learn --outcome <success|failure|workaround|pattern> --title ...`
  records a learning. Auto-embedded for future recall.
- `task-mgr recall --for-task <id>` finds learnings scored for THIS task
  (file pattern + task type + error pattern matches).
- `task-mgr recall --query "<text>"` runs the vector backend (requires
  Ollama; pass `--allow-degraded` for offline runs).
- `task-mgr apply-learning <id>` and `task-mgr invalidate-learning <id>`
  feed the UCB bandit ranking.
- `task-mgr decisions list` shows pending architectural decisions;
  `task-mgr decisions resolve <id> <letter>` records the choice.

### Never edit `tasks/*.json` directly

The PRD task JSON is the source of truth for the loop engine. Editing
it by hand corrupts loop state (the engine re-imports the file on each
iteration and may revert your edit). Use the CLI subcommands above
plus the `<task-status>` tag — never `Edit` or `Write` the JSON
yourself.

"#
    };
}

/// The LLM-coding guidelines body literal, appended to the workflow body to
/// form the `full` profile.
macro_rules! llm_guidelines_body {
    () => {
        r#"## LLM coding guidelines

These guidelines apply to every code change in this repository regardless
of which agent makes them.

### 1. Think before coding

State assumptions BEFORE writing code. Identify the inputs, the
invariants, the failure modes, and the boundary between "user input"
and "trusted internal state." If a requirement is ambiguous, ask one
clarifying question; do not paper over ambiguity with defensive code.

Read the surrounding context — at minimum the file you are editing, the
direct callers of the function you are changing, and any tests that
exercise it. Code review is cheaper than a revert.

### 2. Simplicity first

Prefer the simplest change that satisfies the requirement. Three
similar lines beats a premature abstraction. A direct function call
beats a trait + impl + factory.

Do NOT add error handling, fallbacks, or validation for scenarios that
cannot happen. Trust framework guarantees. Validate at system
boundaries (user input, external APIs), not at every internal call
site. Do NOT add feature flags or backwards-compatibility shims when
you can just change the code.

### 3. Surgical changes

Change only what the task requires. A bug fix does not need
surrounding cleanup. A one-shot operation does not need a helper
function. A new feature does not need a refactor of adjacent code.

If a refactor is genuinely required, do it as a separate commit (or a
separate task) with its own justification — never bundled into an
unrelated change. Reviewers should be able to read the diff and see
exactly what behavior changed.

### 4. Goal-driven execution

Every change must trace back to an acceptance criterion. Before
committing, check: does this diff move every acceptance criterion
strictly toward "satisfied"? If a line does not, delete it.

Run the scoped quality gate before committing: the linters and tests
filter to the module you touched. Do not skip the gate to save time —
a CI failure costs more than the gate run.

"#
    };
}

/// Workflow-only profile: task-mgr CLI cheat sheet and workflow patterns.
///
/// Project-scoped mirror of §3a of the user-global CLAUDE.md. Safe to render
/// into both `CLAUDE.md` (project notes) and `AGENTS.md` (agent context).
pub const WORKFLOW_TEMPLATE: &str = workflow_body!();

/// Workflow content concatenated with the LLM-coding guidelines.
///
/// By construction (both halves emitted by `workflow_body!` /
/// `llm_guidelines_body!`), `FULL_TEMPLATE.starts_with(WORKFLOW_TEMPLATE)`
/// holds for every byte of `WORKFLOW_TEMPLATE`.
pub const FULL_TEMPLATE: &str = concat!(workflow_body!(), llm_guidelines_body!());

/// Marker token unique to the workflow profile, used by tests to verify that
/// `--profile workflow` rendered the workflow content (rather than just
/// "produces non-empty output").
pub const WORKFLOW_PROFILE_TOKEN: &str = "task-mgr workflow";

/// Marker token unique to the full profile (only present in the LLM
/// guidelines section that workflow does not include), used by tests to
/// verify that `--profile full` includes the LLM-coding guidelines.
pub const FULL_PROFILE_TOKEN: &str = "LLM coding guidelines";

/// Available profiles for `task-mgr enhance <agents|show>`.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Default,
    clap::ValueEnum,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum EnhanceProfile {
    /// task-mgr CLI cheat sheet + workflow patterns (project-scoped §3a).
    #[default]
    Workflow,
    /// Workflow profile plus general LLM-coding guidelines.
    Full,
}

impl EnhanceProfile {
    /// Render the profile body that goes inside the marker block.
    pub fn body(self) -> &'static str {
        match self {
            EnhanceProfile::Workflow => WORKFLOW_TEMPLATE,
            EnhanceProfile::Full => FULL_TEMPLATE,
        }
    }
}
