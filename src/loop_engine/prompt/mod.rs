//! Prompt assembly for the autonomous agent loop.
//!
//! Three-builder layout:
//! - [`core`]: bedrock section helpers shared by every builder. Each helper is
//!   self-contained and testable in isolation.
//! - [`sequential`]: today's sequential prompt builder; the canonical
//!   single-task path used by `run_iteration`. Public symbols
//!   [`build_prompt`], [`BuildPromptParams`], and [`PromptResult`] are
//!   re-exported here so `loop_engine::prompt::build_prompt` keeps working
//!   for every existing call site.
//! - [`slot`]: parallel-wave builder that produces a `Send`-safe
//!   [`slot::SlotPromptBundle`] for spawning across worker threads. Composes
//!   the same `core` helpers as `sequential` so wave mode can no longer
//!   silently drift away from the sequential prompt's contract.

pub mod core;
pub mod sequential;
pub mod slot;

pub use sequential::{BuildPromptParams, PromptResult, build_prompt};
