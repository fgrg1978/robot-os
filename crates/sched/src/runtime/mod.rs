//! Scheduler runtime — RFC-0002 modular runtime layer.
//!
//! Mirrors `crates/drivers/src/runtime/` for schedulers. In Phase 1
//! only two backends are wired (Legacy + APS); the remaining policy
//! variants are reserved enum slots so the API is stable when their
//! full dispatch path lands (Phase 2+).

pub mod registry;
