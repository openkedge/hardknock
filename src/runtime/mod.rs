// SPDX-License-Identifier: Apache-2.0

//! Deterministic, evidence-guided control for live agent actions.

mod benchmark;
mod context;
mod model;
mod policy;
mod scenario;

pub use benchmark::*;
pub use context::*;
pub use model::*;
pub use policy::*;
pub use scenario::*;

pub const RUNTIME_POLICY_VERSION: &str = "hardknock.runtime-policy.v1";
pub const RUNTIME_SCENARIO_SCHEMA: &str = "hardknock.scenario.v1";
