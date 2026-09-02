// SPDX-License-Identifier: Apache-2.0

//! Local execution, immutable evidence, and controlled empirical learning.
#![cfg(unix)]

pub mod agent;
pub mod application;
pub mod assurance;
pub mod bridge;
pub mod budget;
pub mod cancellation;
pub mod capability;
pub mod cli;
pub mod core;
pub mod curriculum;
pub mod development;
pub mod dojo;
pub mod effects;
pub mod epistemic;
pub mod error;
pub mod evaluation;
pub mod experience;
pub mod experiment;
pub mod experimentation;
pub mod explanation;
pub mod federation;
pub mod integrations;
pub mod learning_loop;
pub mod lesson;
pub mod perturbation;
pub mod process;
pub mod reflection;
pub mod resilience;
pub mod retrieval;
pub mod runtime;
pub mod store;
pub mod tool;
pub mod tool_benchmark;
pub mod tool_runtime;
/// Compatibility alias for callers that prefer a plural module name.
pub mod tools {
    pub use crate::tool::*;
}
pub mod validation;
pub mod workflow;

pub use error::{Error, Result};
