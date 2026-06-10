//! `task-mgr models` command family (provider-first config, FR-009).
//!
//! Verbs (all writes target the project `.task-mgr/config.json`, round-tripped
//! through `serde_json::Value` so unknown keys survive):
//! - `init [--force-replace-legacy] [--dry-run]` — write the FR-001 default
//!   `models`/`routing` block; the migration deletes the four legacy keys.
//! - `show` — full routing table + anchor-derived difficulty→model mapping +
//!   crash-escalation cost note + codex route-only note + legacy banner.
//! - `list [--remote] [--refresh]` — reverse-lookup of the merged provider
//!   ladders; `--remote` consults the Anthropic catalog (opt-in gated).
//! - `set-anchor <tier>`, `enable|disable <provider>`,
//!   `set-tier|unset-tier <provider> <tier> [model]`,
//!   `set-effort <provider> <difficulty> [effort]`,
//!   `set-fallback|unset-fallback <provider> [target]`,
//!   `route|unroute <prefix> [--provider <p>] [--tier <t>]`.

pub mod api;
pub mod cache;
pub mod ensure_default;
pub mod picker;

mod handlers;
pub use handlers::{
    ListOpts, handle_init, handle_list, handle_route, handle_set_anchor, handle_set_effort,
    handle_set_enabled, handle_set_fallback, handle_set_tier, handle_show, handle_unroute,
    handle_unset_fallback, handle_unset_tier, write_default_block_with_anchor,
};
