# Contributing to task-mgr

Thank you for your interest in contributing to task-mgr! This guide will help you get started with development, understand our code standards, and navigate the contribution process.

## Table of Contents

- [Development Environment Setup](#development-environment-setup)
- [Code Style Guidelines](#code-style-guidelines)
- [Testing Requirements](#testing-requirements)
- [Architecture Overview](#architecture-overview)
- [Pull Request Process](#pull-request-process)
- [Commit Message Conventions](#commit-message-conventions)
- [Issue Reporting Guidelines](#issue-reporting-guidelines)

## Development Environment Setup

### Prerequisites

- **Rust 1.70+** (stable toolchain)
- **SQLite** (bundled via rusqlite, no system install required)

### Clone and Build

```bash
# Clone the repository
git clone https://github.com/your-org/task-mgr.git
cd task-mgr

# Build the project
cargo build

# Run tests to verify setup
cargo test
```

### Development Commands

```bash
# Build in release mode
cargo build --release

# Run the CLI directly during development
cargo run -- init --from-json tests/fixtures/sample_prd.json

# Run with verbose logging
cargo run -- -v list
```

### IDE Setup

**VS Code** with rust-analyzer extension is recommended. Settings that work well:

```json
{
  "rust-analyzer.check.command": "clippy",
  "rust-analyzer.check.extraArgs": ["--", "-D", "warnings"]
}
```

**IntelliJ IDEA** with the Rust plugin also works well.

## Code Style Guidelines

### Linting Requirements

All code must pass linting with zero warnings:

```bash
# Type checking
cargo check

# Linting (warnings treated as errors)
cargo clippy -- -D warnings

# Format check
cargo fmt -- --check
```

### Formatting

We use `rustfmt` with default settings. Run before committing:

```bash
cargo fmt
```

### Naming Conventions

- **Structs/Enums**: PascalCase (e.g., `TaskStatus`, `LearningOutcome`)
- **Functions/Methods**: snake_case (e.g., `get_next_task`, `record_learning`)
- **Constants**: SCREAMING_SNAKE_CASE (e.g., `DEFAULT_DECAY_THRESHOLD`)
- **Modules**: snake_case, matching file names

### Error Handling

- Use `TaskMgrResult<T>` (alias for `Result<T, TaskMgrError>`) for all fallible functions
- Create specific error variants in `src/error.rs` for domain errors
- Include actionable context in error messages

```rust
// Good: specific error with context
TaskMgrError::InvalidTransition {
    task_id: "US-001".to_string(),
    from: TaskStatus::Todo,
    to: TaskStatus::Done,
    hint: "Use `next --claim` to start the task first".to_string(),
}

// Avoid: generic errors without guidance
anyhow::anyhow!("Invalid status")
```

### Documentation

- Add `///` doc comments to public API functions and types
- Include `# Examples` in doc comments for complex functions
- Comments explain *why*, not *what* (the code shows what)

## Testing Requirements

### Test Structure

```
tests/
├── fixtures/           # JSON fixtures for testing
│   └── sample_prd.json
├── error_handling.rs   # Error case coverage
├── import_export.rs    # JSON round-trip tests
├── learnings.rs        # Learnings system tests
└── task_selection.rs   # Smart selection algorithm tests

src/
└── commands/
    └── *.rs           # Unit tests in #[cfg(test)] modules
```

### Running Tests

```bash
# Run all tests
cargo test

# Run specific test module
cargo test --test learnings

# Run tests with output
cargo test -- --nocapture

# Run single test by name
cargo test test_ucb_score_calculation
```

### Writing Tests

1. **Unit tests**: Place in `#[cfg(test)]` module at bottom of source file
2. **Integration tests**: Place in `tests/` directory
3. **Fixtures**: Use `tests/fixtures/` for test data files

Test patterns to follow:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup_test_db() -> (TempDir, Connection) {
        let temp = TempDir::new().unwrap();
        let conn = open_test_connection(temp.path()).unwrap();
        create_schema(&conn).unwrap();
        (temp, conn)
    }

    #[test]
    fn test_feature_does_expected_thing() {
        let (_temp, conn) = setup_test_db();
        // Arrange
        // Act
        // Assert
    }
}
```

### Coverage Guidelines

- All new features must include tests
- Bug fixes should include regression tests
- Aim for coverage of happy path + error cases
- Critical paths (task selection, state transitions) require thorough testing

## Architecture Overview

### Directory Structure

```
src/
├── main.rs           # CLI entry point, output formatting
├── cli.rs            # Clap CLI definitions
├── lib.rs            # Library exports
├── error.rs          # Error types (TaskMgrError)
├── db/
│   ├── connection.rs # SQLite connection with pragmas
│   ├── schema.rs     # Table definitions, migrations
│   ├── lock.rs       # File locking for concurrent access
│   └── migrations.rs # Schema migration framework
├── models/
│   ├── task.rs       # Task struct, TaskStatus enum
│   ├── run.rs        # Run tracking
│   ├── learning.rs   # Learning struct
│   └── ...
├── commands/
│   ├── init.rs       # JSON import
│   ├── next.rs       # Smart task selection
│   ├── complete.rs   # Task completion
│   └── ...
└── learnings/
    ├── crud.rs       # Learning CRUD operations
    └── recall.rs     # Pattern matching retrieval
```

### Key Patterns

1. **Command Pattern**: Each CLI command has a corresponding function in `src/commands/`
2. **Result Structs**: Commands return typed result structs (e.g., `CompleteResult`) for testability
3. **Format Functions**: Separate `format_text()` functions for human-readable output
4. **Lock Management**: Acquire `LockGuard` before write operations

### Database

- SQLite with WAL mode for crash safety
- File locking via `fs2` for concurrent access
- Schema migrations in `src/db/migrations.rs`

## Pull Request Process

### Before Opening a PR

1. **Create a branch** from `main`:
   ```bash
   git checkout -b feature/your-feature-name
   ```

2. **Make your changes** following the style guidelines

3. **Run all quality checks**:
   ```bash
   cargo check && cargo clippy -- -D warnings && cargo test && cargo fmt -- --check
   ```

4. **Write/update tests** for your changes

5. **Update documentation** if adding new features

### PR Guidelines

- **Title**: Clear, descriptive (e.g., "Add bulk operations for fail command")
- **Description**: Explain what and why, reference related issues
- **Size**: Keep PRs focused - one feature or fix per PR
- **Tests**: Include tests that cover the changes

### Review Process

1. Open PR against `main`
2. CI runs automatically (clippy, tests)
3. Maintainer reviews code
4. Address feedback with additional commits
5. Squash merge after approval

### What We Look For in Review

- [ ] Code compiles and passes tests
- [ ] Changes follow existing patterns
- [ ] Error handling is appropriate
- [ ] Tests cover the changes
- [ ] Documentation is updated if needed

## Commit Message Conventions

We follow [Conventional Commits](https://www.conventionalcommits.org/):

```
<type>(<scope>): <description>

[optional body]

[optional footer]
```

### Types

- `feat`: New feature
- `fix`: Bug fix
- `docs`: Documentation only
- `refactor`: Code change that neither fixes a bug nor adds a feature
- `test`: Adding or updating tests
- `chore`: Maintenance tasks

### Examples

```
feat(commands): add bulk operations for fail command

fix(next): handle empty task list without panic

docs: update README with new commands

test(learnings): add UCB score calculation tests

chore: update dependencies
```

### Scope (Optional)

Use the module or area being changed:
- `commands`, `db`, `models`, `learnings`, `cli`

### Task References

If working from a PRD, include the task ID:

```
feat: [US-115] Add bulk operations for task lifecycle commands
```

## Issue Reporting Guidelines

### Bug Reports

Include:

1. **Version**: Output of `task-mgr --version` (or git commit)
2. **Environment**: OS, Rust version
3. **Steps to reproduce**: Exact commands run
4. **Expected behavior**: What should happen
5. **Actual behavior**: What actually happened
6. **Error messages**: Full error output, stack traces

### Feature Requests

Include:

1. **Use case**: Why do you need this feature?
2. **Proposed solution**: How would it work?
3. **Alternatives considered**: Other approaches you've thought of

### Good First Issues

Look for issues labeled `good-first-issue` - these are suitable for newcomers and include extra context.

---

## Questions?

- Open a [discussion](https://github.com/your-org/task-mgr/discussions) for general questions
- Open an [issue](https://github.com/your-org/task-mgr/issues) for bugs or feature requests
