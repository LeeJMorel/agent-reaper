//! Shared model for reaper's tools: the rules file and its lint, the agent event log and
//! the state folded from it, where files live, idle timeouts, and memento mori. The engine
//! and the console (crates/reaper) read rules and events through this crate.

pub mod events;
pub mod idle;
pub mod lint;
pub mod mori;
pub mod paths;
pub mod rules;
pub mod state;
pub mod tail;

/// The example rules file, used to create a first `rules.yaml`.
pub const EXAMPLE_RULES: &str = include_str!("../../../examples/rules.yaml");
