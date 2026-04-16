//! `task-mgr models` command family.
//!
//! Subcommands:
//! - `list [--remote] [--refresh]` — print known model IDs. Remote fetch is
//!   gated on `ANTHROPIC_API_KEY` + `TASK_MGR_USE_API=1`; falls back silently
//!   to the hardcoded constants in [`crate::loop_engine::model`] otherwise.
//! - `set-default [<model>] [--project]` — pin a default model (user config
//!   by default, `--project` writes to `.task-mgr/config.json`). Interactive
//!   when `<model>` omitted.
//! - `unset-default [--project]` — remove the default.
//! - `show` — print the resolved default and where it came from.

pub mod api;
pub mod cache;
pub mod ensure_default;
pub mod picker;

mod handlers;
pub use handlers::{
    handle_list, handle_set_default, handle_show, handle_unset_default, DefaultSource, ListOpts,
    SetDefaultOpts, UnsetDefaultOpts,
};
