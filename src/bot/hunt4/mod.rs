//! Hunt4 live bot runtime.
//!
//! This module owns target selection, route planning, movement dispatch, attack dispatch,
//! teleport escape decisions, tactical memory, and runtime diagnostics.

pub mod actions;
pub mod backend;
pub mod candidate;
pub mod context;
pub mod intent;
pub mod memory;
pub mod model;
pub mod observe;
pub mod perception;
pub mod plan;
pub mod planner;
pub mod policy;
pub mod route;
pub mod runtime;
pub mod score;
pub mod skill_cd;
pub mod state;
pub mod step;
pub mod targeting;
pub mod tick;
pub mod world;
