//! Prompt assembly for the autonomous agent loop.
//!
//! # Three-builder layout
//!
//! - [`core`]: bedrock section helpers shared by every builder (steering,
//!   key-decisions block, learnings, source/synergy, task envelope, etc.).
//!   Each helper is self-contained and unit-testable in isolation; both
//!   higher-level builders compose them so sequential and wave mode can never
//!   silently drift apart on prompt contents.
//! - [`sequential`]: the canonical single-task builder used by `run_iteration`.
//!   Public symbols [`build_prompt`], [`BuildPromptParams`], and
//!   [`PromptResult`] are re-exported here so existing call sites (and
//!   `cargo doc` consumers outside the crate) keep resolving
//!   `loop_engine::prompt::build_prompt` after the split.
//! - [`slot`]: parallel-wave builder. Produces a `Send`-safe
//!   [`slot::SlotPromptBundle`] (a fully owned `String` + small metadata)
//!   that worker threads consume directly. Composes the same `core` helpers
//!   as `sequential`, so any new section added to the sequential prompt must
//!   also be wired into `slot` — there is no second source of truth.
//!
//! # Main-thread bundle rule
//!
//! Wave mode MUST build [`slot::SlotPromptBundle`] on the main thread (inside
//! `run_parallel_wave`'s spawn loop, before each `thread::spawn`) and move
//! the owned bundle into the worker. **Slot worker threads never read from
//! `&Connection`** — `rusqlite::Connection` is `!Send`, and every learnings
//! / source / synergy lookup that feeds the prompt must run on the main
//! thread before the spawn (see learnings #1893 / #1852 / #1871). A
//! compile-time `Send` assertion on `SlotPromptBundle` backstops this rule;
//! adding a non-`Send` field (e.g. `Rc`, `RefCell`, `Connection`) breaks
//! the build by design.

pub mod core;
pub mod sequential;
pub mod slot;

pub use sequential::{BuildPromptParams, PromptResult, build_prompt};
