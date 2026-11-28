//! Shared plumbing for every VIctory crate and tool.
//!
//! * [`config`]: where the user's files live (never inside the repository).
//! * [`policy`]: the runtime half of rule #1, refusing to write
//!   game-derived data into the source tree.
//! * [`logging`]: one tracing setup for all binaries.
//! * [`ab`]: switches that alternate inside a run, to measure both ways.
//! * [`session`]: the run's structured log and summary (`logs/`).
//! * [`alloc_count`]: whether the counting allocator counts
//!   (`VICTORY_COUNT_ALLOCS`).

pub mod ab;
pub mod alloc_count;
pub mod config;
pub mod logging;
pub mod policy;
pub mod session;

pub use config::Config;
pub use policy::{PolicyError, ensure_outside_repo};
