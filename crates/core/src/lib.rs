//! Simulator-independent core of the PauseAtToD plugin.
//!
//! Nothing in this crate talks to X-Plane: the plugin samples the sim into an
//! [`AircraftState`] and feeds it to a [`Monitor`], which decides when to pause.
//! That keeps every heuristic unit-testable on any machine.

pub mod geo;
pub mod settings;
pub mod trigger;

pub use settings::{AltRef, Direction, Settings};
pub use trigger::{AircraftState, ConditionKind, ConditionStatus, Fired, Monitor, Waypoint};
